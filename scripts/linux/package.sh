#!/usr/bin/env bash
# Собирает Linux .deb и .AppImage для v4.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="${1:-4.0.0-alpha.1}"
OUT="$ROOT/dist"
APPDIR="$OUT/ScreenRecorder.AppDir"
PKGROOT="$OUT/deb-root"
ARCH="amd64"
APPIMAGE_ARCH="x86_64"

cd "$ROOT"

# В CI исходники не содержат больших бинарников; скачиваем их при сборке.
if [[ ! -x bin/linux/ffmpeg || ! -x bin/linux/ffprobe ]]; then
  echo "[build] downloading static ffmpeg for Linux..."
  mkdir -p bin/linux /tmp/ffmpeg-linux
  curl -L --fail --retry 3     -o /tmp/ffmpeg-linux/ffmpeg.tar.xz \
    "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
  tar -xJf /tmp/ffmpeg-linux/ffmpeg.tar.xz -C /tmp/ffmpeg-linux --strip-components=1
  install -m755 /tmp/ffmpeg-linux/ffmpeg bin/linux/ffmpeg
  install -m755 /tmp/ffmpeg-linux/ffprobe bin/linux/ffprobe
fi

APPIMAGETOOL="${APPIMAGETOOL:-$ROOT/tools/appimagetool-x86_64.AppImage}"
if [[ ! -x "$APPIMAGETOOL" ]]; then
  echo "[build] downloading appimagetool..."
  mkdir -p "$ROOT/tools"
  curl -L --fail --retry 3 -o "$APPIMAGETOOL" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
  chmod +x "$APPIMAGETOOL"
fi

cargo build --release -p screenrecorder-app -p screenrecorder-ml-worker -p screenrecorder-diarize

rm -rf "$APPDIR" "$PKGROOT"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$OUT"

install -m755 target/release/screenrecorder "$APPDIR/usr/bin/"
install -m755 target/release/screenrecorder-ml "$APPDIR/usr/bin/"
install -m755 target/release/screenrecorder-diarize "$APPDIR/usr/bin/"
install -m755 bin/linux/ffmpeg "$APPDIR/usr/bin/ffmpeg"
install -m755 bin/linux/ffprobe "$APPDIR/usr/bin/ffprobe"
install -m644 assets/icon.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/screenrecorder.png"
cat > "$APPDIR/usr/share/applications/screenrecorder.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Screen Recorder
Comment=Запись экрана, системного звука и микрофона
Exec=screenrecorder
Icon=screenrecorder
Terminal=false
Categories=AudioVideo;Recorder;
StartupWMClass=screenrecorder
DESKTOP
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:$PATH"
exec "$HERE/usr/bin/screenrecorder" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"
cp "$APPDIR/usr/share/applications/screenrecorder.desktop" "$APPDIR/"
cp "$APPDIR/usr/share/icons/hicolor/256x256/apps/screenrecorder.png" "$APPDIR/screenrecorder.png"

if ! ARCH="$APPIMAGE_ARCH" "$APPIMAGETOOL" "$APPDIR" "$OUT/ScreenRecorder-$VERSION-$APPIMAGE_ARCH.AppImage"; then
  ARCH="$APPIMAGE_ARCH" "$APPIMAGETOOL" --appimage-extract-and-run "$APPDIR" "$OUT/ScreenRecorder-$VERSION-$APPIMAGE_ARCH.AppImage"
fi

mkdir -p "$PKGROOT/DEBIAN" "$PKGROOT/usr/bin" "$PKGROOT/usr/share/applications" \
  "$PKGROOT/usr/share/icons/hicolor/256x256/apps"
install -m755 target/release/screenrecorder "$PKGROOT/usr/bin/"
install -m755 target/release/screenrecorder-ml "$PKGROOT/usr/bin/"
install -m755 target/release/screenrecorder-diarize "$PKGROOT/usr/bin/"
install -m644 assets/icon.png "$PKGROOT/usr/share/icons/hicolor/256x256/apps/screenrecorder.png"
install -m644 "$APPDIR/usr/share/applications/screenrecorder.desktop" "$PKGROOT/usr/share/applications/"
cat > "$PKGROOT/DEBIAN/control" <<CONTROL
Package: screenrecorder
Version: $VERSION
Section: video
Priority: optional
Architecture: $ARCH
Depends: ffmpeg, pipewire-bin, x11-utils, libxkbcommon0, libwayland-client0, libx11-6, libxcb1
Maintainer: Freekazoid
Description: Screen Recorder 4
 Запись экрана, системного звука и микрофона с распознаванием речи.
CONTROL
dpkg-deb --build --root-owner-group "$PKGROOT" "$OUT/screenrecorder_${VERSION}_${ARCH}.deb"

echo "Готово: $OUT"
