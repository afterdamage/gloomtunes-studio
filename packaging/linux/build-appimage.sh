#!/usr/bin/env bash
# Builds GloomTunes-Studio-<version>-x86_64.AppImage from an existing build.
#
#   cargo build --profile dist --locked -p gt-app
#   packaging/linux/build-appimage.sh            # writes dist/
#
# PROFILE=release packages a `--release` build instead (bigger: it keeps line tables).
#
# Needs desktop-file-validate (Ubuntu: desktop-file-utils). Uses appimagetool (downloaded once into target/appimage-tools/ when not on PATH; the
# version is pinned and its checksum verified). System libraries (ALSA, X11/Wayland, GL) are
# not bundled: they are present on every desktop Ubuntu 22.04+ and must match the host's
# drivers anyway.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
version="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="gt-app"))')"
profile="${PROFILE:-dist}"
bin="target/$profile/gloomtunes"
[ -x "$bin" ] || { echo "build first: cargo build --profile $profile --locked -p gt-app" >&2; exit 1; }

appdir="target/appimage/GloomTunes.AppDir"
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/256x256/apps" "$appdir/usr/share/icons/hicolor/scalable/apps" \
  "$appdir/usr/share/metainfo" "$appdir/usr/share/mime/packages"
install -m755 "$bin" "$appdir/usr/bin/gloomtunes"
install -m644 packaging/linux/gloomtunes.desktop "$appdir/usr/share/applications/"
install -m644 packaging/linux/gloomtunes.desktop "$appdir/gloomtunes.desktop"
install -m644 packaging/gloomtunes.png "$appdir/usr/share/icons/hicolor/256x256/apps/"
install -m644 packaging/gloomtunes.svg "$appdir/usr/share/icons/hicolor/scalable/apps/"
install -m644 packaging/gloomtunes.png "$appdir/gloomtunes.png"
ln -sf gloomtunes.png "$appdir/.DirIcon"
install -m644 packaging/linux/gloomtunes.metainfo.xml \
  "$appdir/usr/share/metainfo/io.github.afterdamage.gloomtunes.appdata.xml"
install -m644 packaging/linux/gloomtunes-mime.xml "$appdir/usr/share/mime/packages/gloomtunes.xml"
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/gloomtunes" "$@"
RUN
chmod 755 "$appdir/AppRun"

tool="$(command -v appimagetool || true)"
if [ -z "$tool" ]; then
  tools="target/appimage-tools"
  tool="$tools/appimagetool-x86_64.AppImage"
  if [ ! -x "$tool" ]; then
    mkdir -p "$tools"
    url="https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage"
    sha="46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1"
    curl -fsSL -o "$tool.part" "$url"
    echo "$sha  $tool.part" | sha256sum -c -
    mv "$tool.part" "$tool"
    chmod 755 "$tool"
  fi
fi

mkdir -p dist
out="dist/GloomTunes-Studio-$version-x86_64.AppImage"
# --appimage-extract-and-run: works where FUSE is unavailable (containers, CI).
ARCH=x86_64 "$tool" --appimage-extract-and-run "$appdir" "$out"
echo "wrote $out"
