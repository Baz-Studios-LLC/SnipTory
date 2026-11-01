#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version="$(tr -d '\r\n' < VERSION)"
tag="v$version"
metadata="0.0.$((10#$version))"
stage="$PWD/dist/stage-$tag-linux-x64"
output="$PWD/dist/$tag-linux-x64"
test ! -e "$stage" && test ! -e "$output"
if [ "${1:-}" != '--skip-build' ]; then cargo build --release --locked; fi
mkdir -p "$stage" "$output"
cp target/release/sniptory "$stage/"
cp /usr/bin/wl-copy "$stage/"
cp LICENSE licenses/Slint-Royalty-Free.md "$stage/"
cp /usr/share/doc/wl-clipboard/copyright "$stage/WL_CLIPBOARD_COPYRIGHT"
mkdir -p "$stage/wl-clipboard-source"
clipboard_version="$(dpkg-query -W -f='${Version}' wl-clipboard)"
(cd "$stage/wl-clipboard-source" && apt-get source --download-only "wl-clipboard=$clipboard_version")
cp /usr/share/common-licenses/GPL-3 "$stage/wl-clipboard-source/GPL-3.txt"
printf 'Corresponding source for the bundled wl-copy executable. Ubuntu source archives include upstream code, distribution patches and debian/rules build instructions. Extract with dpkg-source -x *.dsc, install the package build dependencies, then build with dpkg-buildpackage.\n' > "$stage/wl-clipboard-source/README.txt"
python3 scripts/Generate-Notices.py "$stage/THIRD_PARTY_NOTICES.txt"
.tools/vpk/vpk pack --packId BazStudios.SnipTory --packVersion "$metadata" --packDir "$stage" --mainExe sniptory --packTitle SnipTory --packAuthors 'Baz Studios LLC' --channel linux-x64 --runtime linux-x64 --icon assets/sniptory.png --releaseNotes "release-notes/$tag.md" --outputDir "$output"
python3 scripts/Normalize-Linux-Release.py "$output" "$tag"
image="$output/SnipTory-$tag-linux-x64.AppImage"
chmod +x "$image"
APPIMAGE_EXTRACT_AND_RUN=1 "$image" --smoke-test
