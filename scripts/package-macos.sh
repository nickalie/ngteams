#!/usr/bin/env bash
# Assembles ngteams.app for macOS (Apple silicon) from built binaries, with GStreamer's runtime inside, and packs it as
# a zip and a dmg:
#
#   scripts/package-macos.sh --binaries <dir with ngteams, ngteams-webview, ngteamsctl> --gstreamer <prefix> \
#       --version <major.minor.patch[.run]> --out <dir>
#
# - ngteams.app/Contents/MacOS holds the three binaries; Contents/Frameworks/GStreamer.framework/Versions/1.0 holds
#   the runtime's libraries, the plugins packaging/macos/gstreamer-plugins.txt names and the plugin scanner, in the
#   runtime's own layout (GStreamer finds its plugins and scanner relative to its library).
# - Every binary's rpath to the build's GStreamer prefix is replaced by the bundle's Frameworks.
# - The bundle is signed ad hoc (`codesign -s -`): no Apple Developer ID, so macOS asks once before it first opens
#   (right-click > Open). Notarization needs a Developer ID certificate and an App Store Connect key (docs/ci.md).
# - Info.plist: id com.nickalie.ngteams, the camera and microphone usage descriptions, the msteams: URL scheme.
set -euo pipefail

binaries= gstreamer= version= out=
while (($#)); do
    case $1 in
        --binaries) binaries=$2; shift 2 ;;
        --gstreamer) gstreamer=$2; shift 2 ;;
        --version) version=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        *) echo "unknown argument $1" >&2; exit 2 ;;
    esac
done
[[ -n $binaries && -n $gstreamer && -n $version && -n $out ]] || {
    echo "usage: scripts/package-macos.sh --binaries <dir> --gstreamer <prefix> --version <v> --out <dir>" >&2
    exit 2
}
root=$(cd "$(dirname "$0")/.." && pwd)
gstreamer=$(cd "$gstreamer" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
app="$out/ngteams.app"
[[ ! -e $app ]] || { echo "$app exists: remove it first" >&2; exit 1; }
short=$(echo "$version" | cut -d. -f1-3)

contents="$app/Contents"
frameworks="$contents/Frameworks/GStreamer.framework/Versions/1.0"
mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks/lib/gstreamer-1.0" "$frameworks/libexec/gstreamer-1.0"
for binary in ngteams ngteams-webview ngteamsctl; do
    install -m 0755 "$binaries/$binary" "$contents/MacOS/$binary"
done

# the runtime's shared libraries (plugins load them; the app links a few), then only the named plugins
find "$gstreamer/lib" -maxdepth 1 -name '*.dylib' -exec cp -a {} "$frameworks/lib/" \;
while read -r plugin; do
    [[ -z $plugin || $plugin == \#* ]] && continue
    source="$gstreamer/lib/gstreamer-1.0/libgst$plugin.dylib"
    [[ -f $source ]] || { echo "the GStreamer runtime has no plugin $plugin" >&2; exit 1; }
    cp "$source" "$frameworks/lib/gstreamer-1.0/"
done < "$root/packaging/macos/gstreamer-plugins.txt"
cp "$gstreamer/libexec/gstreamer-1.0/gst-plugin-scanner" "$frameworks/libexec/gstreamer-1.0/"

for binary in "$contents/MacOS"/*; do
    while read -r rpath; do
        install_name_tool -delete_rpath "$rpath" "$binary"
    done < <(otool -l "$binary" | awk '/LC_RPATH/ { getline; getline; print $2 }' | grep -F "$gstreamer" || true)
done
for binary in ngteams ngteams-webview; do
    install_name_tool -add_rpath "@executable_path/../Frameworks/GStreamer.framework/Versions/1.0/lib" \
        "$contents/MacOS/$binary" 2>/dev/null || true
done
# a link to a library outside the bundle and the system would fail on the user's Mac
if otool -L "$contents/MacOS"/* | grep -F "$gstreamer"; then
    echo "a binary still names the build's GStreamer prefix" >&2
    exit 1
fi

iconset="$out/ngteams.iconset"
rm -rf "$iconset" && mkdir "$iconset"
source_icon="$root/packaging/icons/hicolor/512x512/apps/com.nickalie.ngteams.png"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source_icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    [[ $double -le 512 ]] && sips -z "$double" "$double" "$source_icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$contents/Resources/ngteams.icns"
rm -rf "$iconset"

cat > "$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>ngteams</string>
    <key>CFBundleExecutable</key><string>ngteams</string>
    <key>CFBundleIconFile</key><string>ngteams</string>
    <key>CFBundleIdentifier</key><string>com.nickalie.ngteams</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>ngteams</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$short</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.business</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>NSCameraUsageDescription</key><string>ngteams uses the camera for your video in Teams calls and meetings.</string>
    <key>NSMicrophoneUsageDescription</key><string>ngteams uses the microphone for your voice in Teams calls and meetings.</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>Microsoft Teams link</string>
            <key>CFBundleURLSchemes</key><array><string>msteams</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST
plutil -lint "$contents/Info.plist" >/dev/null

# ad hoc: nested code first, the bundle last
find "$contents/Frameworks" -type f \( -name '*.dylib' -o -perm -u+x \) -exec codesign --force --sign - {} \;
for binary in ngteamsctl ngteams-webview; do
    codesign --force --sign - "$contents/MacOS/$binary"
done
codesign --force --sign - --identifier com.nickalie.ngteams "$app"
codesign --verify --strict --deep "$app"

stem="ngteams-$version-macos-arm64"
ditto -c -k --keepParent "$app" "$out/$stem.zip"
staging="$out/dmg"
rm -rf "$staging" && mkdir "$staging"
cp -R "$app" "$staging/" && ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname ngteams -srcfolder "$staging" -format UDZO "$out/$stem.dmg"
rm -rf "$staging"
(cd "$out" && shasum -a 256 "$stem.zip" "$stem.dmg")
