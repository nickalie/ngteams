#!/usr/bin/env bash
# Vendor the crates.io crates we patch into vendor/<crate> with build/patches/<crate> applied, for the workspace's
# [patch.crates-io]. The GPUI crates came with M0 Task 16 (approach (b) of the video spike), taken from agterm-rs
# (scripts/vendor-gpui.sh) with its external-surfaces patches only, unchanged:
#   gpui-pre       `ExternalSurface` (a dmabuf or CPU pixels) as the Linux payload of `PaintSurface`, and
#                  `Window::paint_external_surface` (0001); and, our own, no line break right after opening
#                  punctuation (UAX #14 LB14), so `[[account]]` wraps whole (0002); a path prompt's title and file
#                  filters (0003), so the account picture's chooser shows the picture files; a file manager's drag
#                  put in mouse input mode (0004), so a drop after typed keys reaches the element under the pointer;
#                  right-to-left and mixed text by the Unicode Bidirectional Algorithm, wrapped in text order and drawn
#                  and hit-tested in visual order per row, with a paragraph direction in the text style (0005);
#                  a keystroke event that tells if it is a held key's repeat (0006), so the chat list drops the
#                  repeats of a held Enter or Space; the test platform's window title readable (0007), so a test
#                  checks what a window is titled
#   gpui-pre-wgpu  the renderer's side: dmabufs imported through wgpu-hal Vulkan and copied on the GPU, CPU pixels
#                  uploaded into a texture kept per surface, by damaged rectangle (0001); and, our own, a weight or
#                  style the family has no face of drawn anyway: a variable face at that weight, else synthesized (0002)
#   gpui-pre-linux our own: the HTML another program puts on the Wayland clipboard beside its text, kept as the
#                  item's metadata for the rich-text editor's paste (0001); immediate reads of local clipboard
#                  writes, source generations, and validated private rich-copy recovery after a manager handoff (0002);
#                  every form a clipboard offer holds read into one item: text, picture and uri-list files (0003);
#                  cancel key repeat when its focused window closes or its key is released without focus (0004);
#                  the path prompt's title and filters on the file chooser portal (0005); a file drop accepted on
#                  its enter with that enter's serial, read from a URI list or a sandboxed program's portal key, and
#                  a drop that comes before the files were read delivered once they are (0006); a window's
#                  activation signed with the last press's serial, a key's too, so niri raises a window for a
#                  shortcut (0007)
#   gpui-pre-windows our own: test-support `contains` on the DirectX atlas so atlas-inspection tests query
#                  the real atlas (0001); the path prompt's title and filters on the file open dialog (0002); a
#                  right-to-left glyph run placed where DirectWrite draws it, and a line whose first strong letter is
#                  right to left laid out as a right-to-left paragraph (0003)
#   libwebrtc      our own: `RtpTransceiver::set_direction`, which LiveKit's bindings leave out though `webrtc-sys`
#                  binds it (0001): a group call's screen goes out on the mixer's sharing line, turned to send before
#                  the answer (crates/teams-media/src/rtc.rs); preserve optional candidate-pair RTT (0002), so
#                  absent values remain unknown and a measured zero is retained; `RtpSender::generate_key_frame` on
#                  the safe API (0003). Keep these patches until upstream supports them (0.4.0 still leaves out
#                  set_direction).
#   webrtc-sys     our own: libwebrtc's `RtpSenderInterface::GenerateKeyFrame` bound (0001), so a shared screen can
#                  answer the Teams mixer's repeated keyframe request that libwebrtc ignores
#                  (crates/teams-media/src/session/keyframes.rs). Its prebuilt libwebrtc stays the one it downloads.
#   image-webp    bound cumulative Huffman group, tree and lookup-table allocations to 16 MiB per lossless decoder,
#                  including compressed alpha, and check VP8 dimensions before allocating frame buffers;
#                  the image wrapper does not forward its allocation limit.
#
# Each source is the crates.io archive of the crate's VERSION, verified against SHA256 below (the release's crates.io
# checksum: the `cksum` of its line in https://index.crates.io/<prefix>/<crate>, also what Cargo.lock recorded for the
# unpatched crate). It is unpacked verbatim apart from its Cargo.lock and, for gpui-pre, its examples/ directory, then
# patched. Bump a VERSION and its sum together with the workspace's pin of that crate, run this, and check that the
# patches still apply.
#
# Usage: scripts/vendor-crates.sh           (re)create vendor/<crate> for every crate below
#        scripts/vendor-crates.sh --check   rebuild into a scratch dir and fail if a vendored crate differs
set -euo pipefail
shopt -s nullglob

declare -A VERSION=(
  [image-webp]="0.2.4"
  [gpui-pre]="0.3.9"
  [gpui-pre-wgpu]="0.3.9"
  [gpui-pre-linux]="0.3.9"
  [gpui-pre-windows]="0.3.9"
  [libwebrtc]="0.3.55"
  [webrtc-sys]="0.3.52"
)
declare -A SHA256=(
  [image-webp]="525e9ff3e1a4be2fbea1fdf0e98686a6d98b4d8f937e1bf7402245af1909e8c3"
  [gpui-pre]="34f61c1cfd36fc9c436af09308c963822ae27a45a84feb21a77ba5c9e051f1c0"
  [gpui-pre-wgpu]="5b1cdb26c1503bf9662ee8fbc5ab0b44b2deeee4b5c12dc67491ba6fb5fc544b"
  [gpui-pre-linux]="e28213c3fde6f13e91ba0b74b90cbaadec9373e36a031eda98561d31059870f3"
  [gpui-pre-windows]="513d4f0fc7d596d3bdef2a73a524ffb83dabc95bf484f2855a698e7f431290a9"
  [libwebrtc]="c0399e372f69d3bb3ad8d51f6354f3090c57317de4f61521aa2f5a542c06fe1f"
  [webrtc-sys]="bae66695bde7b250a9e5f7976db6015e30fa06033afc1745e5499d55aa51612a"
)
CRATES=(gpui-pre gpui-pre-wgpu gpui-pre-linux gpui-pre-windows libwebrtc webrtc-sys image-webp)

root="$(cd "$(dirname "$0")/.." && pwd)"

die() {
  echo "$*" >&2
  exit 1
}

mode=vendor
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  -h | --help) sed -n '1d; /^#/!q; s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit 0 ;;
  *) die "unknown argument: $1 (see --help)" ;;
esac

scratch="$(mktemp -d "${TMPDIR:-/tmp}/ngteams-vendor-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The verified crates.io source of $1 with its patches applied, in $scratch/built/$1.
built() {
  local crate="$1" version="${VERSION[$1]}" archive sum patch
  archive="$scratch/$crate-$version.crate"
  curl -sSfL -A ngteams-dev -o "$archive" "https://static.crates.io/crates/$crate/$crate-$version.crate"
  sum="$(sha256sum "$archive" | cut -d' ' -f1)"
  [[ "$sum" == "${SHA256[$crate]}" ]] || die "$crate $version: sha256 $sum, expected ${SHA256[$crate]}"
  mkdir -p "$scratch/unpack" "$scratch/built"
  tar -xzf "$archive" -C "$scratch/unpack"
  mv "$scratch/unpack/$crate-$version" "$scratch/built/$crate"
  rm -f "$scratch/built/$crate/Cargo.lock"
  if [[ "$crate" == gpui-pre ]]; then
    rm -rf "$scratch/built/$crate/examples"
  fi
  for patch in "$root/build/patches/$crate"/*.patch; do
    patch -s -p1 -d "$scratch/built/$crate" <"$patch" || die "$(basename "$patch") does not apply to $crate $version"
  done
}

for crate in "${CRATES[@]}"; do
  version="${VERSION[$crate]}"
  built "$crate"
  target="$root/vendor/$crate"
  case "$mode" in
    check)
      diff -r "$scratch/built/$crate" "$target" >&2 || die "vendor/$crate differs from $crate $version + build/patches/$crate"
      echo "vendor/$crate is $crate $version + build/patches/$crate"
      ;;
    vendor)
      mkdir -p "$root/vendor"
      rm -rf "$target.new"
      mv "$scratch/built/$crate" "$target.new"
      rm -rf "$target"
      mv "$target.new" "$target"
      echo "vendored $crate $version into $target"
      ;;
  esac
done
