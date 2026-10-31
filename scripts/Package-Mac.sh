#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version="$(tr -d '\r\n' < VERSION)"
case "$(uname -m)" in arm64) arch=arm64;; x86_64) arch=x64;; *) printf 'Unsupported Mac architecture\n' >&2; exit 1;; esac
tag="v$version"
metadata="0.0.$((10#$version))"
stage="$PWD/dist/stage-$tag-macos-$arch"
output="$PWD/dist/$tag-macos-$arch"
if [ -e "$stage" ] || [ -e "$output" ]; then printf 'Version already staged; use a clean checkout or advance VERSION.\n' >&2; exit 1; fi
if [ "${1:-}" != '--skip-build' ]; then cargo build --release --locked; fi
app="$stage/SnipTory.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$output" "$stage/AppIcon.iconset"
cp target/release/sniptory "$app/Contents/MacOS/sniptory"
cp LICENSE licenses/Slint-Royalty-Free.md "$app/Contents/Resources/"
python3 scripts/Generate-Notices.py "$app/Contents/Resources/THIRD_PARTY_NOTICES.txt"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/sniptory.png --out "$stage/AppIcon.iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/sniptory.png --out "$stage/AppIcon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$stage/AppIcon.iconset" -o "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.bazstudios.sniptory</string>
<key>CFBundleName</key><string>SnipTory</string>
<key>CFBundleDisplayName</key><string>SnipTory</string>
<key>CFBundleExecutable</key><string>sniptory</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$((10#$version))</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>LSUIElement</key><true/>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
<key>NSAppleEventsUsageDescription</key><string>SnipTory copies captured GIF files to your clipboard.</string>
</dict></plist>
EOF
plutil -lint "$app/Contents/Info.plist"
# Ad-hoc signing is for this test build only; Developer ID/notarization require
# the owner's signing credentials. No Gatekeeper settings are altered.
.tools/vpk/vpk pack --packId BazStudios.SnipTory --packVersion "$metadata" --packDir "$app" --mainExe sniptory --packTitle SnipTory --packAuthors 'Baz Studios LLC' --channel "osx-$arch" --runtime "osx-$arch" --noInst --signAppIdentity - --releaseNotes "release-notes/$tag.md" --outputDir "$output"
python3 scripts/Normalize-Mac-Release.py "$output" "$tag" "$arch"
