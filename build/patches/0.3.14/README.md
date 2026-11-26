# Patches for gpui-pre 0.3.14

This directory holds the patches for the next GPUI snapshot, `gpui-pre*` 0.3.14. The workspace still builds 0.3.9
with `build/patches/<crate>`. `scripts/vendor-crates.sh` reads `NEXT_VERSION` and `NEXT_SHA256` for this set.

- `scripts/vendor-crates.sh --check` verifies the 0.3.14 archives and applies these patches in a scratch directory.
- `scripts/vendor-crates.sh --next DIR` writes the patched 0.3.14 crates to `DIR/<crate>`. `DIR` must be outside
  the repository and the main checkout, and no `DIR/<crate>` can exist yet, so a second run never deletes port work.
  Use it to port a patch: apply the 0.3.9 patch there, fix the rejects, and write the new patch here.

The script applies the patches of a crate in name order, with no fuzz and no backup files: a patch that needs fuzz
fails, because this set has no vendored tree to compare with. A ported patch keeps its 0.3.9 name and number. A dropped
patch leaves a gap in the numbers.

## Status

A patch with the status "here" is a copy of its 0.3.9 patch, with its offsets refreshed against 0.3.14. It has the same
added and removed lines. A patch with the status "ported" has changes for the 0.3.14 source, and the table names them.

| Crate | Patch | Status |
|---|---|---|
| gpui-pre | 0001-external-surfaces | here |
| gpui-pre | 0002-no-break-after-opening-punctuation | dropped: upstream breaks lines by UAX #14 (`icu_segmenter`, zed #64624). Keep the `[[account]]` wrap test. |
| gpui-pre | 0003-path-prompt-title-and-filters | here |
| gpui-pre | 0004-file-drag-is-pointer-input | here |
| gpui-pre | 0005-bidirectional-text | ported. 0.3.14 paints a layer with no bounds, so the bidi paint functions give none. The Greek, Hebrew and Arabic word characters for wrapping are gone: UAX #14 breaks between their words. `tests/bidirectional_text.rs` is new. |
| gpui-pre | 0006-keystroke-event-is-held | ported. The keystroke observers and interceptors of 0.3.14 have a different `retain` call. `tests/keystroke_event_is_held.rs` is new. |
| gpui-pre | 0007-test-window-title | here |
| gpui-pre-wgpu | 0001-dmabuf-external-surfaces | ported: wgpu 30. The pipeline uses the depth attachment of the main pass with no depth write, as the macOS surface pipeline does. The vertex shader clips the quad to the content mask with `clip_to_mask`, because 0.3.14 removed `distance_from_clip_rect`. |
| gpui-pre-wgpu | 0002-font-weight-and-style-synthesis | here |
| gpui-pre-wgpu | 0003-native-backend-env | here |
| gpui-pre-linux | 0001-html-clipboard | here |
| gpui-pre-linux | 0002-local-clipboard-ownership | here. The same added and removed lines, with 3 lines of context. The 0.3.9 patch has no context lines, thus `patch` can put an added line at a wrong place. |
| gpui-pre-linux | 0003-clipboard-pictures-and-files | here |
| gpui-pre-linux | 0004-cancel-key-repeat-on-focus-loss | here |
| gpui-pre-linux | 0005-path-prompt-title-and-filters | here |
| gpui-pre-linux | 0006-file-drops-from-every-file-manager | here. The fuzz was in a context line of an import (`ActivationContext` is new upstream). |
| gpui-pre-linux | 0007-activation-serial-of-the-last-press | dropped: upstream signs the activation token with the last key or pointer press and the surface of that press (`ActivationContext`, zed #64058). Under niri, a key press in one window lets it activate another window. |
| gpui-pre-windows | 0001-atlas-inspection | here |
| gpui-pre-windows | 0002-path-prompt-title-and-filters | here |
| gpui-pre-windows | 0003-right-to-left-glyph-runs | ported. 0.3.14 splits a line at U+2028 and U+2029, thus the first strong letter of the whole line gives the direction of each part. A tab takes its place from the hit test of the layout. It needs `gpui::first_strong_direction` and `gpui::TextDirection` from the port of gpui-pre 0005. |

When all patches are ported or dropped, the bump moves these files to `build/patches/<crate>` and this set becomes
the vendored set.
