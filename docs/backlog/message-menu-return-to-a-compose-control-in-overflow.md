---
worth: later
status: open
where: crates/teams-ui/src/keyboard.rs (Return), crates/teams-ui/src/chat_view/menu_keys.rs
added: 2026-11-23
---
# A message's menu gives the keys back to a compose control that is drawn no more

A message's menu that closes gives the keys back to where they were when it opened. When that place was in the
message list and the list draws it no more, the list takes the keys (`Return::keep_in`). A control of the compose box
does not have this fallback. Example: a compose toolbar button has the focus, a click opens a message's menu, Tab goes
into the menu, the window becomes narrower and the button goes into the toolbar's overflow, then Escape. The keys go
to a handle that is not drawn, and they act from the window root until the focus moves again.

To do: before `Return::give_back` gives the focus to a saved handle, make sure that the last frame drew it. Give a
fallback for each area: the list for its rows, the compose editor for the compose box's controls. The review of task
721 (round 16) found it.
