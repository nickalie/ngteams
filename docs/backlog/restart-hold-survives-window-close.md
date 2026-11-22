---
worth: later
status: open
where: crates/teams-ui/src/desktop.rs
added: 2026-11-22
---
# A restart hold for channel files does not survive the close of the main window

`Desktop::update_waiting` finds unsent messages and channel file uploads and recycles only in the shells of the open
main window. When the user closes the window to the tray, its shells and their boxes go away. A channel file recycle
that is still running then holds no restart. If the user selects Restart to update before the recycle request gets
to the server, the file can stay in the channel's folder.

To do: keep the count of channel uploads and recycles that are not complete in account state that stays when the
window closes (as the calls model does). Then let `update_waiting` read that state for `Waiting::Files`. The review of
task 706 (round 7) found it. This limit was there before the hold reasons were separate.
