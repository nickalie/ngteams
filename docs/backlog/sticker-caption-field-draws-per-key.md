---
worth: later
status: open
where: crates/teams-ui (views that use gpui-component's inputs), gpui-base 0.8
added: 2026-11-25
---
# A key in a kit text field draws the whole window up to three times

GPUI draws the whole window before it dispatches a key event when the window is dirty. A full draw takes 75 to 105 ms
in a debug build, because no view of the app is cached. gpui-component's multi-line input makes the window dirty
again after almost each key event:

- it pauses the cursor blink at each keystroke, and that pause notifies the input (`blink_cursor.rs`, `pause`);
- it notifies itself in paint when the cursor moves (`input/base/element.rs`, the check of `geometry_changed`).

A counter in the key dispatch found 616 full draws for 190 keys in such a field, and 192 for the same typing in the
compose box (the app's own `Editor`). "Edit sticker" stalled 11 to 55 s after fast typing in a debug build. Its caption
fields now use the `Editor`, and the stall is gone (11 ms). Other kit text fields of the app can still lag in the same
way with long, fast typing.

To do, when a kit field lags:

- Cache the large views that do not change while a field is typed (the sidebar, the chat list) with `cached()`.
- Patch gpui-base: no notify from a pause of a cursor that is already paused and visible.
