#!/usr/bin/env bash
# Build the macOS app bundle and a .dmg for the Telekin viewer.
#
#   packaging/macos/build.sh
#
# Must run on macOS: the bundle is plain files, but `hdiutil` (the .dmg) and
# `codesign` only exist there. Produces a universal binary when both targets
# are installed (`rustup target add aarch64-apple-darwin x86_64-apple-darwin`),
# otherwise the native one.
#
# Unsigned. Gatekeeper will show "unidentified developer" on first open; the
# operator right-clicks the app and chooses Open, once. Signing and
# notarisation need an Apple Developer account and are left to the release
# owner — see the comment at the bottom.
set -euo pipefail

cd "$(dirname "$0")/../.."
version="$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
app="dist/Telekin.app"
dmg="dist/telekin-${version}-macos.dmg"

echo "== telekin ${version} (macos) =="

# Universal if we can, native if we cannot.
if rustup target list --installed | grep -q aarch64-apple-darwin \
   && rustup target list --installed | grep -q x86_64-apple-darwin; then
    cargo build --release -p telekin --target aarch64-apple-darwin
    cargo build --release -p telekin --target x86_64-apple-darwin
    mkdir -p target/universal
    lipo -create \
        target/aarch64-apple-darwin/release/telekin \
        target/x86_64-apple-darwin/release/telekin \
        -output target/universal/telekin
    bin=target/universal/telekin
else
    cargo build --release -p telekin
    bin=target/release/telekin
fi

rm -rf "$app" "$dmg"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/telekin"
sed "s/@VERSION@/${version}/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

# .icns from the PNG set; iconutil is part of macOS.
iconset="$(mktemp -d)/telekin.iconset"
mkdir -p "$iconset"
for s in 16 32 128 256; do
    cp "packaging/icons/telekin-${s}.png" "$iconset/icon_${s}x${s}.png"
done
cp packaging/icons/telekin-32.png  "$iconset/icon_16x16@2x.png"
cp packaging/icons/telekin-64.png  "$iconset/icon_32x32@2x.png"
cp packaging/icons/telekin-256.png "$iconset/icon_128x128@2x.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/telekin.icns"

# Ad-hoc signature so the binary at least runs on Apple silicon, which refuses
# entirely unsigned code. Replace `-` with a Developer ID to sign properly.
codesign --force --deep --sign - "$app"

mkdir -p dist
hdiutil create -volname "Telekin ${version}" -srcfolder "$app" -ov -format UDZO "$dmg"

echo
echo "built:"
ls -1 "$app" "$dmg"

# To notarise (needs an Apple Developer account):
#   codesign --force --deep --options runtime --sign "Developer ID Application: ..." "$app"
#   xcrun notarytool submit "$dmg" --apple-id ... --team-id ... --password ... --wait
#   xcrun stapler staple "$dmg"
