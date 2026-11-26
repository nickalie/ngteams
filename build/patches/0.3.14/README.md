# Patches for gpui-pre 0.3.14

This directory holds the patches for the next GPUI snapshot, `gpui-pre*` 0.3.14. The workspace still builds 0.3.9
with `build/patches/<crate>`. `scripts/vendor-crates.sh` reads `NEXT_VERSION` and `NEXT_SHA256` for this set.

- `scripts/vendor-crates.sh --check` verifies the 0.3.14 archives and applies these patches in a scratch directory.
- `scripts/vendor-crates.sh --next DIR` writes the patched 0.3.14 crates to `DIR/<crate>`. `DIR` must be outside
  the repository. Use it to port a patch: apply the 0.3.9 patch there, fix the rejects, and write the new patch here.

The script applies the patches of a crate in name order. A ported patch keeps its 0.3.9 name and number. A dropped
patch leaves a gap in the numbers.

## Status

The patches here are copies of the 0.3.9 patches, with their offsets refreshed against 0.3.14. They have the same
added and removed lines.

| Crate | Patch | Status |
|---|---|---|
| gpui-pre | 0001-external-surfaces | to port (external surfaces and wgpu 30 task) |
| gpui-pre | 0002-no-break-after-opening-punctuation | to drop: upstream breaks lines by UAX #14 (`icu_segmenter`, zed #64624). Keep the `[[account]]` wrap test. |
| gpui-pre | 0003-path-prompt-title-and-filters | here |
| gpui-pre | 0004-file-drag-is-pointer-input | here |
| gpui-pre | 0005-bidirectional-text | to port (text task) |
| gpui-pre | 0006-keystroke-event-is-held | to port (text task) |
| gpui-pre | 0007-test-window-title | here |
| gpui-pre-wgpu | 0001-dmabuf-external-surfaces | to port (external surfaces and wgpu 30 task) |
| gpui-pre-wgpu | 0002-font-weight-and-style-synthesis | here |
| gpui-pre-wgpu | 0003-native-backend-env | here |
| gpui-pre-linux | 0001-html-clipboard | here |
| gpui-pre-linux | 0002-local-clipboard-ownership | to port (Linux and Windows platform task) |
| gpui-pre-linux | 0003-clipboard-pictures-and-files | to port after 0002 (Linux and Windows platform task) |
| gpui-pre-linux | 0004-cancel-key-repeat-on-focus-loss | here |
| gpui-pre-linux | 0005-path-prompt-title-and-filters | here |
| gpui-pre-linux | 0006-file-drops-from-every-file-manager | here. The fuzz was in a context line of an import (`ActivationContext` is new upstream). |
| gpui-pre-linux | 0007-activation-serial-of-the-last-press | to drop, probably: upstream signs the activation token with the last key or pointer press and its surface (`ActivationContext`, zed #64058). The Linux and Windows platform task must confirm this with niri. |
| gpui-pre-windows | 0001-atlas-inspection | here |
| gpui-pre-windows | 0002-path-prompt-title-and-filters | here |
| gpui-pre-windows | 0003-right-to-left-glyph-runs | to port (Linux and Windows platform task) |

When all patches are ported or dropped, the bump moves these files to `build/patches/<crate>` and this set becomes
the vendored set.
