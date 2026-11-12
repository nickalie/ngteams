#!/usr/bin/env bash
# Checks what scripts/package-macos.sh made, as a user gets it, without opening a window or touching an account:
#
#   scripts/check-macos-package.sh --out <the package script's --out dir> [--version <v>]
#
# - the zip and the dmg are there; the zip unpacks (ditto) to ngteams.app, and the dmg holds ngteams.app beside a
#   link to /Applications (drag to install);
# - the app is signed (`codesign --verify --strict --deep`), from the zip and from the dmg;
# - Info.plist: the id, the executable, the msteams: scheme, the camera and microphone descriptions, macOS 14, the icon;
#   a lab build's own id (com.nickalie.ngteams.lab) and no URL scheme;
# - the three programs, GStreamer's runtime under Contents/Frameworks/gstreamer (libraries, every plugin
#   packaging/macos/gstreamer-plugins.txt names, the scanner) and no dotted folder there (codesign takes it for a
#   bundle);
# - no Mach-O file in the bundle links or searches a path outside the bundle and the system (the user's Mac has no
#   GStreamer of its own), and every @rpath library the programs link is in the bundle;
# - the updater's marker and release; `ngteams --build-identity` and `ngteamsctl --help` run from the bundle.
#
# macOS only. Exits 1 on the first failed check, after saying which.
set -euo pipefail
out='' version=''
while (($#)); do
    case $1 in
        --out) out=$2; shift 2 ;;
        --version) version=$2; shift 2 ;;
        *) echo "unknown argument $1" >&2; exit 2 ;;
    esac
done
[[ -n $out ]] || { echo "usage: scripts/check-macos-package.sh --out <dir> [--version <v>]" >&2; exit 2; }
[[ $(uname -s) == Darwin ]] || { echo "macOS only" >&2; exit 2; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=$(cd "$out" && pwd)
passed=0
ok() { echo "ok   $*"; passed=$((passed + 1)); }
fail() { echo "FAIL $*" >&2; exit 1; }

shopt -s nullglob
zips=("$out"/ngteams-*-macos-arm64.zip)
dmgs=("$out"/ngteams-*-macos-arm64.dmg)
shopt -u nullglob
[[ ${#zips[@]} -eq 1 && ${#dmgs[@]} -eq 1 ]] || fail "one ngteams-<v>-macos-arm64 zip and dmg in $out"
stem=$(basename "${zips[0]}" .zip)
[[ $(basename "${dmgs[0]}" .dmg) == "$stem" ]] || fail "the zip and the dmg have one version"
found=${stem#ngteams-}
found=${found%-macos-arm64}
[[ -z $version || $found == "$version" ]] || fail "the package's version is $found, not $version"
version=$found
ok "zip and dmg for $version"

work=$(mktemp -d "${TMPDIR:-/tmp}/ngteams-package-check.XXXXXX")
mount="$work/dmg"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
cleanup() {
    if [[ -d $mount ]] && mount | grep -qF " $mount "; then hdiutil detach -quiet "$mount" || true; fi
    # running a program from the copy registers it with LaunchServices (an msteams: handler): take it back out
    [[ -d $work/zip/ngteams.app ]] && "$lsregister" -u "$work/zip/ngteams.app" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

ditto -x -k "${zips[0]}" "$work/zip"
app="$work/zip/ngteams.app"
[[ -d $app && $(find "$work/zip" -mindepth 1 -maxdepth 1 | wc -l) -eq 1 ]] || fail "the zip holds ngteams.app alone"
ok "the zip unpacks to ngteams.app"
codesign --verify --strict --deep "$app" || fail "codesign verifies the zip's app"
ok "the zip's app is signed ($(codesign -dv "$app" 2>&1 | sed -n 's/^Signature=//p'))"

plist="$app/Contents/Info.plist"
key() { plutil -extract "$1" raw -o - "$plist" 2>/dev/null; }
marker=$(env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH "$app/Contents/MacOS/ngteams" --build-marker) ||
    fail "ngteams --build-marker runs from the bundle"
if [[ $marker == lab ]]; then
    [[ $(key CFBundleIdentifier) == com.nickalie.ngteams.lab ]] || fail "a lab build's id com.nickalie.ngteams.lab"
    ! plutil -extract CFBundleURLTypes xml1 -o - "$plist" >/dev/null 2>&1 || fail "a lab build claims no URL scheme"
    scheme="its own id, no URL scheme"
else
    [[ $(key CFBundleIdentifier) == com.nickalie.ngteams ]] || fail "bundle id com.nickalie.ngteams"
    [[ $(key CFBundleURLTypes.0.CFBundleURLSchemes.0) == msteams ]] || fail "the msteams: scheme is claimed"
    scheme="msteams:"
fi
[[ $(key CFBundleExecutable) == ngteams ]] || fail "the executable is ngteams"
[[ $(key CFBundleVersion) == "$version" ]] || fail "CFBundleVersion is $version"
[[ -n $(key NSCameraUsageDescription) && -n $(key NSMicrophoneUsageDescription) ]] ||
    fail "the camera and microphone say why they are asked for"
[[ $(key LSMinimumSystemVersion) == 14.0 ]] || fail "macOS 14 or later"
[[ -f $app/Contents/Resources/$(key CFBundleIconFile).icns ]] || fail "the icon file is in Resources"
ok "Info.plist ($marker build): id, $scheme, executable, version, camera and microphone, macOS 14, icon"

for program in ngteams ngteams-webview ngteamsctl; do
    [[ -x $app/Contents/MacOS/$program ]] || fail "Contents/MacOS/$program"
done
ok "the three programs"
runtime="$app/Contents/Frameworks/gstreamer"
[[ -x $runtime/libexec/gst-plugin-scanner ]] || fail "GStreamer's plugin scanner"
[[ -n $(find "$runtime/lib" -name 'libgstreamer-1.0*.dylib' -print -quit) ]] || fail "GStreamer's core library"
while read -r plugin; do
    [[ -z $plugin || $plugin == \#* ]] && continue
    [[ -f $runtime/plugins/libgst$plugin.dylib ]] || fail "the plugin $plugin is in the bundle"
done < "$root/packaging/macos/gstreamer-plugins.txt"
dotted=$(find "$app/Contents/Frameworks" -type d -name '*.*' | head -1)
[[ -z $dotted ]] || fail "no dotted folder in Frameworks: $dotted"
ok "GStreamer inside: $(find "$runtime/lib" -type f -name '*.dylib' | wc -l | tr -d ' ') libraries, $(find "$runtime/plugins" -name '*.dylib' | wc -l | tr -d ' ') plugins, the scanner"

# every Mach-O file: what it links and where it searches stay in the bundle or the system
machos=0
while IFS= read -r -d '' file; do
    file -b "$file" | grep -q '^Mach-O' || continue
    machos=$((machos + 1))
    while read -r linked; do
        case $linked in
            /System/* | /usr/lib/* | @rpath/* | @loader_path/* | @executable_path/*) ;;
            *) fail "${file#"$app"/} links $linked" ;;
        esac
    # a universal file has a header line per architecture ("<file> (architecture arm64):"), each ending in ':'
    done < <(otool -L "$file" | grep -v ':$' | awk '{print $1}')
    while read -r searched; do
        case $searched in
            @loader_path* | @executable_path* | /usr/lib/*) ;;
            *) fail "${file#"$app"/} searches $searched" ;;
        esac
    done < <(otool -l "$file" | awk '/LC_RPATH/ { getline; getline; print $2 }')
done < <(find "$app" -type f -print0)
ok "$machos Mach-O files link and search only the bundle and the system"
for program in ngteams ngteams-webview; do
    while read -r linked; do
        name=${linked#@rpath/}
        [[ -f $runtime/lib/$name ]] || fail "$program's $linked is in Frameworks/gstreamer/lib"
    done < <(otool -L "$app/Contents/MacOS/$program" | awk '$1 ~ /^@rpath\// {print $1}')
done
ok "the programs' @rpath libraries are all in the bundle"

[[ $(cat "$app/Contents/Resources/ngteams/update-format") == macos-app ]] || fail "update-format is macos-app"
[[ $(cat "$app/Contents/Resources/ngteams/release") == "$version" ]] || fail "release is $version"
ok "the updater's marker and release"
# display-free: the build identity and the control client's help touch no account or credential store; no
# DYLD_* variable, so GStreamer is the bundle's
identity=$(env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH "$app/Contents/MacOS/ngteams" --build-identity) ||
    fail "ngteams --build-identity runs from the bundle"
env -u DYLD_FALLBACK_LIBRARY_PATH -u DYLD_LIBRARY_PATH "$app/Contents/MacOS/ngteamsctl" --help > /dev/null ||
    fail "ngteamsctl --help runs from the bundle"
ok "ngteams --build-identity ($(echo "$identity" | tr '\n' ' '| sed 's/ $//')) and ngteamsctl --help run"

mkdir "$mount"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mount" "${dmgs[0]}" || fail "the dmg mounts"
[[ -d $mount/ngteams.app ]] || fail "the dmg holds ngteams.app"
[[ -L $mount/Applications && $(readlink "$mount/Applications") == /Applications ]] ||
    fail "the dmg has a link to /Applications to drag the app to"
codesign --verify --strict --deep "$mount/ngteams.app" || fail "codesign verifies the dmg's app"
hdiutil detach -quiet "$mount"
ok "the dmg holds the signed ngteams.app beside /Applications"
echo "$passed checks passed"
