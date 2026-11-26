# The QA walk does not press a real Tab to a file card

The keyboard step of the walk of task 793 (`crates/ngteams/tests/support/linux/qa_file_grid_video.rs`, `keyboard`)
accepts 0 Tab presses. In its chat, Enter in the list gives the keys to the lone video card itself, so the walk passes
with "Escape, Enter, 0 Tab, then Enter". The view tests in `crates/teams-ui/tests/chat_view/files.rs` move the focus
with `window.focus_next` and `focus_prev`, because their window has no kit root. Thus nothing presses the real Tab of
the kit's `Root` to a file card in the app window.

Add a second file card (or a message with a file before the lone video) to the walk's chat, so that the walk must press
Tab at least once to reach the card. Then require 1 or more presses for that card. Starting the loop at 1 is not
sufficient: Tab from the lone card goes to its own buttons. Run the walk in the UI sandbox
(`GPUI_SMOKE_REQUIRE=1 cargo test -p ngteams --test qa_file_grid_window`).

Found by the review of task 849 (round 1).
