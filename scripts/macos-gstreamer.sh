#!/usr/bin/env bash
# Installs the official GStreamer macOS runtime pinned in packaging/macos/media-pins.json (the version the Windows
# build pins) into a private prefix: no administrator, no Homebrew, nothing outside <prefix>.
#
#   scripts/macos-gstreamer.sh <prefix> [<downloaded .pkg>]
#
# The package is checked against the pinned SHA-256 and unpacked with `pkgutil --expand-full`; only the components the
# app's player and devices use are kept. The runtime's libraries name each other through `@rpath`, and GStreamer finds
# its plugins and scanner relative to its own library, so the prefix can live anywhere. The runtime package carries
# no pkg-config files (those come with the devel package, which also hard-codes /Library/Frameworks), so this writes
# the few the Rust bindings ask for. The libraries are named `@rpath/...`: a development binary finds them through
# DYLD_FALLBACK_LIBRARY_PATH=<prefix>/lib (scripts/run-lab-macos.py sets it), ngteams.app through its own rpath
# (scripts/package-macos.sh).
#
# Afterwards: export PKG_CONFIG_PATH=<prefix>/lib/pkgconfig
set -euo pipefail

prefix=${1:?usage: scripts/macos-gstreamer.sh <prefix> [<downloaded .pkg>]}
package=${2:-}
root=$(cd "$(dirname "$0")/.." && pwd)
pins="$root/packaging/macos/media-pins.json"
pin() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$pins" "$1"; }
version=$(pin gstreamer_version)
url=$(pin gstreamer_url)
sha256=$(pin gstreamer_sha256)

mkdir -p "$prefix"
prefix=$(cd "$prefix" && pwd)
if [[ -f "$prefix/.complete" && $(cat "$prefix/.complete") == "$sha256" ]]; then
    echo "GStreamer $version already in $prefix"
    exit 0
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/ngteams-gstreamer.XXXXXX")
trap 'rm -rf "$work"' EXIT

if [[ -z "$package" ]]; then
    package="$work/gstreamer.pkg"
    # the server drops long transfers and can't resume them: try the whole download again
    for attempt in 1 2 3 4 5; do
        curl --fail --silent --show-error --location --http1.1 --output "$package" "$url" && break
        echo "download attempt $attempt failed" >&2
        [[ $attempt == 5 ]] && exit 1
    done
fi
echo "$sha256  $package" | shasum -a 256 -c - >/dev/null
pkgutil --expand-full "$package" "$work/expanded"

# The framework's components the app uses: the core and its base plugins, playback, the codecs (libav's too) and the
# effects (volume, audio filters, deinterlacing); not Python, GTK, the editing services, devtools or network sources.
for component in osx-framework base-system-1.0 base-crypto gstreamer-1.0-core gstreamer-1.0-system \
    gstreamer-1.0-playback gstreamer-1.0-codecs gstreamer-1.0-codecs-restricted gstreamer-1.0-codecs-gpl \
    gstreamer-1.0-codecs-gpl-restricted gstreamer-1.0-libav gstreamer-1.0-effects gstreamer-1.0-encoding; do
    payload="$work/expanded/$component-$version-universal.pkg/Payload"
    [[ -d "$payload" ]] || { echo "the package has no $component" >&2; exit 1; }
    ditto "$payload" "$prefix"
done

mkdir -p "$prefix/lib/pkgconfig"
write_pc() {
    local name=$1 library=$2 requires=$3 pc_version=$4
    cat > "$prefix/lib/pkgconfig/$name.pc" <<PC
prefix=$prefix
libdir=\${prefix}/lib

Name: $name
Description: $name from the official GStreamer $version macOS runtime (written by scripts/macos-gstreamer.sh)
Version: $pc_version
Requires: $requires
Libs: -L\${libdir} -l$library
Cflags:
PC
}
glib_version=$(otool -L "$prefix/lib/libglib-2.0.dylib" | awk '/libglib-2.0.0.dylib/ { gsub(/[),]/, ""); print $NF; exit }')
# glib's current version is 100 * minor + 1 + micro (8201.4 is 2.82.4)
glib_minor=$(( ${glib_version%%.*} / 100 ))
glib_micro=$(echo "$glib_version" | cut -d. -f2)
glib="2.$glib_minor.$glib_micro"
write_pc glib-2.0 glib-2.0 "" "$glib"
write_pc gobject-2.0 gobject-2.0 "glib-2.0" "$glib"
write_pc gmodule-2.0 gmodule-2.0 "glib-2.0" "$glib"
write_pc gio-2.0 gio-2.0 "gobject-2.0" "$glib"
write_pc gstreamer-1.0 gstreamer-1.0 "glib-2.0 gobject-2.0" "$version"
for part in base app audio video pbutils tag; do
    write_pc "gstreamer-$part-1.0" "gst$part-1.0" "gstreamer-1.0" "$version"
done
echo "$sha256" > "$prefix/.complete"
echo "GStreamer $version in $prefix; build with PKG_CONFIG_PATH=$prefix/lib/pkgconfig"
