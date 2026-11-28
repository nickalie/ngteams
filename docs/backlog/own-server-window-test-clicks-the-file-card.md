---
worth: yes
where: crates/ngteams/tests/support/linux/own_server_nodes_window.rs, crates/ngteams/tests/support/linux/own_server_nodes_app.rs, crates/teams-ui/src/files.rs
added: 2026-11-16
---
# The own-server window test downloads the file through the card

Step 4 of `own_server_nodes_window` checks that `srv-b` can read the file `srv-a` sent on the other node. Its probe
calls `EngineRequests::file_stream` in the app process. It does not click the card. The card's own actions for an own
server's file (a click, "Open in desktop app", "Download": through the engine, `teams_ui::files::EngineFile`) are tested
in `crates/teams-ui/tests/chat_view/files.rs` on GPUI's test platform, with a fake engine.

To test them end to end, the window test needs:

- a probe that gives a selector's bounds in the app's window (`file-more-<message>-0`, `file-menu-download-…`), so
  the parent clicks them with the sandbox's input;
- a known result of the save prompt in the sandbox (no portal: the app saves in the downloads folder), and that
  folder's path in the sandbox, where the parent reads the file and compares its bytes.
