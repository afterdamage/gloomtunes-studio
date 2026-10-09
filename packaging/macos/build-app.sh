#!/usr/bin/env bash
# Builds "GloomTunes Studio.app" and GloomTunes-Studio-<version>-macos-universal.dmg in dist/.
# Runs on macOS (needs lipo, codesign, hdiutil from the Xcode command line tools).
#
#   packaging/macos/build-app.sh
#
# PROFILE (default dist) picks the cargo profile. ARCHS (default "aarch64 x86_64") picks the
# CPUs: both make a universal app that runs natively on Apple Silicon and Intel Macs; one
# (ARCHS=aarch64) is quicker for a local try and names the .dmg after it.
#
# Signing: with MACOS_SIGN_IDENTITY set to a "Developer ID Application: ..." identity in the
# keychain, the app is signed with the hardened runtime and entitlements.plist. Without it the
# app is signed ad hoc, which Apple Silicon needs to run it at all; Gatekeeper then asks the
# user to allow it once (README, "Installing").
# Notarizing: with MACOS_SIGN_IDENTITY, APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD (an
# app-specific password) set, the .dmg is notarized by Apple and the ticket stapled to it.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
version="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="gt-app"))')"
profile="${PROFILE:-dist}"
archs="${ARCHS:-aarch64 x86_64}"
# The oldest macOS the app supports; matches LSMinimumSystemVersion in Info.plist.
export MACOSX_DEPLOYMENT_TARGET=11.0

bins=()
for arch in $archs; do
  target="$arch-apple-darwin"
  if command -v rustup >/dev/null; then rustup target add "$target"; fi
  cargo build --profile "$profile" --locked -p gt-app --target "$target"
  bins+=("target/$target/$profile/gloomtunes")
done
# shellcheck disable=SC2086
set -- $archs
if [ $# -gt 1 ]; then flavor=universal; else flavor="$1"; fi

app="dist/GloomTunes Studio.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create "${bins[@]}" -output "$app/Contents/MacOS/gloomtunes"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
cp packaging/macos/gloomtunes.icns "$app/Contents/Resources/"
printf 'APPL????' > "$app/Contents/PkgInfo"

identity="${MACOS_SIGN_IDENTITY:-}"
if [ -n "$identity" ]; then
  codesign --force --timestamp --options runtime \
    --entitlements packaging/macos/entitlements.plist --sign "$identity" "$app"
else
  codesign --force --sign - "$app"
fi
codesign --verify --strict --verbose=2 "$app"

dmg="dist/GloomTunes-Studio-$version-macos-$flavor.dmg"
stage="target/dmg"
rm -rf "$stage" "$dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname "GloomTunes Studio" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"
rm -rf "$stage"

if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
    xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" --wait
    xcrun stapler staple "$dmg"
  fi
fi
# The .app stays in dist/ for a quick local run; the release uploads only the .dmg.
echo "$dmg"
