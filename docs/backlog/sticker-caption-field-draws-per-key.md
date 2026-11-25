---
worth: later
status: open
where: crates/teams-ui/src/sticker_editor.rs, gpui-component's multi-line input (gpui-base 0.8)
added: 2026-11-25
---
# Edit sticker: each key in the caption field draws the window three times

In a debug build, the QA check `caption_long_and_empty` types 190 characters at about 20 characters each second. The
app answers about 20 s after the last key. The compose box keeps pace with the same typing.

The picture of the editor is not the cause: the app stalls also with no picture draws. GPUI draws the whole window
before it dispatches a key event when the window is dirty. A full draw takes 75 to 105 ms in a debug build, because no
view of the app is cached. The caption field is gpui-component's multi-line input, and it makes the window dirty
again after almost each key event:

- it pauses the cursor blink at each keystroke, and that pause notifies the input (`blink_cursor.rs`, `pause`);
- it notifies itself in paint when the cursor moves (`input/base/element.rs`, the check of `geometry_changed`).

A counter in the key dispatch found 616 full draws for the 190 keys. The compose box, the app's own `Editor`, gave 192.

To do: one of these.

- Cache the large views that do not change while a field is typed (the sidebar, the chat list) with `cached()`.
- Make the caption field one that notifies only when its text changes, for example the compose box's `Editor`.
- Patch gpui-base: no notify from a pause of a cursor that is already paused and visible.
