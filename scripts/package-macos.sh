#!/usr/bin/env bash
# Assembles ngteams.app for macOS (Apple silicon) from built binaries, with GStreamer's runtime inside, and packs it as
# a zip and a dmg:
#
#   scripts/package-macos.sh --binaries <dir with ngteams, ngteams-webview, ngteamsctl> --gstreamer <prefix> \
#       --version <major.minor.patch[.run]> --out <dir>
#
# - ngteams.app/Contents/MacOS holds the three binaries; Contents/Frameworks/gstreamer holds the runtime's libraries
#   (lib/), the plugins packaging/macos/gstreamer-plugins.txt names (plugins/) and the plugin scanner (libexec/). No
#   dotted folder such as gstreamer-1.0: codesign reads one as a bundle. The app names these to GStreamer when it
#   starts from the bundle (crates/ngteams/src/mac_app.rs); the plugins find the libraries by their @loader_path/../lib.
# - Every binary's rpath to the build's GStreamer prefix is replaced by the bundle's Frameworks.
# - Contents/Resources/ngteams/update-format (`macos-app`) and .../release (the version) let a release's app update
#   itself (crates/teams-update); a build without a release version never asks.
# - Signing: with NGTEAMS_MACOS_SIGN_IDENTITY (a Developer ID Application identity in the keychain, its name or hash)
#   every binary is signed with it, the hardened runtime and a secure timestamp, the app's own programs with
#   packaging/macos/ngteams.entitlements (camera, microphone). Without it the bundle is signed ad hoc (`codesign -s -`):
#   macOS asks once before it first opens it (right-click > Open), and the Keychain asks again after each update.
# - Notarization, only with a Developer ID: with NGTEAMS_NOTARY_KEY (an App Store Connect API key's .p8 file),
#   NGTEAMS_NOTARY_KEY_ID and NGTEAMS_NOTARY_ISSUER, the app and then the dmg go to Apple's notary service and get its
#   ticket stapled (docs/ci.md, "macOS signing").
# - Info.plist: id com.nickalie.ngteams, the camera and microphone usage descriptions, the msteams: URL scheme. A lab
#   build (`ngteams --build-marker` says `lab`) gets id com.nickalie.ngteams.lab and no URL scheme, as Windows' lab
#   runs under its own app id: LaunchServices never hands it a Teams link or the personal app's notifications.
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
marker=$(DYLD_FALLBACK_LIBRARY_PATH="$gstreamer/lib" "$binaries/ngteams" --build-marker) ||
    { echo "can't read the build marker of $binaries/ngteams" >&2; exit 1; }
if [[ $marker == lab ]]; then
    bundle_id=com.nickalie.ngteams.lab
    url_types=''
else
    bundle_id=com.nickalie.ngteams
    url_types='    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>Microsoft Teams link</string>
            <key>CFBundleURLSchemes</key><array><string>msteams</string></array>
        </dict>
    </array>'
fi

contents="$app/Contents"
frameworks="$contents/Frameworks/gstreamer"
mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks/lib" "$frameworks/plugins" "$frameworks/libexec"
for binary in ngteams ngteams-webview ngteamsctl; do
    install -m 0755 "$binaries/$binary" "$contents/MacOS/$binary"
done

# the runtime's shared libraries (plugins load them; the app links a few), then only the named plugins
find "$gstreamer/lib" -maxdepth 1 -name '*.dylib' -exec cp -a {} "$frameworks/lib/" \;
while read -r plugin; do
    [[ -z $plugin || $plugin == \#* ]] && continue
    source="$gstreamer/lib/gstreamer-1.0/libgst$plugin.dylib"
    [[ -f $source ]] || { echo "the GStreamer runtime has no plugin $plugin" >&2; exit 1; }
    cp "$source" "$frameworks/plugins/"
done < "$root/packaging/macos/gstreamer-plugins.txt"
cp "$gstreamer/libexec/gstreamer-1.0/gst-plugin-scanner" "$frameworks/libexec/"

for binary in "$contents/MacOS"/*; do
    while read -r rpath; do
        install_name_tool -delete_rpath "$rpath" "$binary"
    done < <(otool -l "$binary" | awk '/LC_RPATH/ { getline; getline; print $2 }' | grep -F "$gstreamer" || true)
done
for binary in ngteams ngteams-webview; do
    install_name_tool -add_rpath "@executable_path/../Frameworks/gstreamer/lib" \
        "$contents/MacOS/$binary" 2>/dev/null || true
done
# a link to a library outside the bundle and the system would fail on the user's Mac
if otool -L "$contents/MacOS"/* | grep -F "$gstreamer"; then
    echo "a binary still names the build's GStreamer prefix" >&2
    exit 1
fi

# the release marker and identity the updater checks
mkdir -p "$contents/Resources/ngteams"
printf 'macos-app\n' > "$contents/Resources/ngteams/update-format"
printf '%s\n' "$version" > "$contents/Resources/ngteams/release"

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
    <key>CFBundleIdentifier</key><string>$bundle_id</string>
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
$url_types
</dict>
</plist>
PLIST
plutil -lint "$contents/Info.plist" >/dev/null

# nested code first, the bundle last; a Developer ID signs with the hardened runtime and a timestamp, else ad hoc
identity=${NGTEAMS_MACOS_SIGN_IDENTITY:-}
if [[ -n $identity ]]; then
    sign=(codesign --force --sign "$identity" --timestamp --options runtime)
else
    sign=(codesign --force --sign -)
fi
entitlements="$root/packaging/macos/ngteams.entitlements"
find "$contents/Frameworks" -type f \( -name '*.dylib' -o -perm -u+x \) -exec "${sign[@]}" {} \;
"${sign[@]}" "$contents/MacOS/ngteamsctl"
"${sign[@]}" --entitlements "$entitlements" "$contents/MacOS/ngteams-webview"
"${sign[@]}" --entitlements "$entitlements" --identifier "$bundle_id" "$app"
codesign --verify --strict --deep "$app"

stem="ngteams-$version-macos-arm64"
notarize() {
    xcrun notarytool submit "$1" --key "$NGTEAMS_NOTARY_KEY" --key-id "$NGTEAMS_NOTARY_KEY_ID" \
        --issuer "$NGTEAMS_NOTARY_ISSUER" --wait --timeout 1h
}
notary=
if [[ -n $identity && -n ${NGTEAMS_NOTARY_KEY:-} && -n ${NGTEAMS_NOTARY_KEY_ID:-} && -n ${NGTEAMS_NOTARY_ISSUER:-} ]]; then
    notary=1
    ditto -c -k --keepParent "$app" "$out/notarize.zip"
    notarize "$out/notarize.zip"
    rm "$out/notarize.zip"
    xcrun stapler staple "$app"
fi
ditto -c -k --keepParent "$app" "$out/$stem.zip"
staging="$out/dmg"
rm -rf "$staging" && mkdir "$staging"
cp -R "$app" "$staging/" && ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname ngteams -srcfolder "$staging" -format UDZO "$out/$stem.dmg"
rm -rf "$staging"
if [[ -n $identity ]]; then
    codesign --force --sign "$identity" --timestamp "$out/$stem.dmg"
fi
if [[ -n $notary ]]; then
    notarize "$out/$stem.dmg"
    xcrun stapler staple "$out/$stem.dmg"
fi
(cd "$out" && shasum -a 256 "$stem.zip" "$stem.dmg")
