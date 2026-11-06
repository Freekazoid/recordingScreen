#!/usr/bin/env bash
# Собирает ScreenRecorder.app для macOS по аналогии с v3:
# .app + zip + опционально dmg, ad-hoc/Developer ID подпись.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script must be run on macOS (Darwin)."
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="${APP_VERSION:-${1:-4.0.0-alpha.2}}"
ARCH="${MACOS_TARGET_ARCH:-$(uname -m)}"
OUT="$ROOT/dist"
APP="$OUT/ScreenRecorder.app"
BUNDLE_ID="com.recordingscreen.app"
FFMPEG_CACHE="$ROOT/bin/macos"
ASSET_TAG="b6.1.1"
BASE_URL="https://github.com/eugeneware/ffmpeg-static/releases/download/${ASSET_TAG}"

case "$ARCH" in
  arm64) TARGETS=("aarch64-apple-darwin"); FF_ASSETS=("darwin-arm64") ;;
  x86_64) TARGETS=("x86_64-apple-darwin"); FF_ASSETS=("darwin-x64") ;;
  universal2)
    TARGETS=("aarch64-apple-darwin" "x86_64-apple-darwin")
    FF_ASSETS=("darwin-arm64" "darwin-x64")
    ;;
  *) echo "Unsupported MACOS_TARGET_ARCH: $ARCH (arm64|x86_64|universal2)"; exit 1 ;;
esac

cd "$ROOT"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT" "$FFMPEG_CACHE"

for target in "${TARGETS[@]}"; do
  cargo build --release --target "$target" \
    -p screenrecorder-app -p screenrecorder-ml-worker -p screenrecorder-diarize
done

# NeMo-воркер опционален: на macOS он тянет ONNX Runtime.
if [[ "${INCLUDE_NEMO:-0}" == "1" ]]; then
  for target in "${TARGETS[@]}"; do
    cargo build --release --target "$target" -p screenrecorder-nemo
  done
fi

# Собирает один Mach-O: для universal2 склеивает слайсы через lipo.
make_universal() {
  local name="$1" dest="$2"
  if [[ "$ARCH" == "universal2" ]]; then
    lipo -create \
      "$ROOT/target/aarch64-apple-darwin/release/$name" \
      "$ROOT/target/x86_64-apple-darwin/release/$name" \
      -output "$dest"
  else
    cp "$ROOT/target/${TARGETS[0]}/release/$name" "$dest"
  fi
  chmod +x "$dest"
}

make_universal screenrecorder "$APP/Contents/MacOS/screenrecorder"
make_universal screenrecorder-ml "$APP/Contents/MacOS/screenrecorder-ml"
make_universal screenrecorder-diarize "$APP/Contents/MacOS/screenrecorder-diarize"
if [[ "${INCLUDE_NEMO:-0}" == "1" ]]; then
  make_universal screenrecorder-nemo "$APP/Contents/MacOS/screenrecorder-nemo"
fi

# Статический ffmpeg/ffprobe; для universal2 склеиваем обе архитектуры.
for tool in ffmpeg ffprobe; do
  parts=()
  for asset in "${FF_ASSETS[@]}"; do
    file="$FFMPEG_CACHE/${tool}-${asset}"
    if [[ ! -f "$file" ]]; then
      echo "[build] downloading ${tool} ${asset}..."
      curl -L --fail --retry 3 \
        "$BASE_URL/${tool}-${asset}.gz" | gzip -dc > "$file"
      chmod +x "$file"
    fi
    parts+=("$file")
  done
  if [[ "$ARCH" == "universal2" ]]; then
    lipo -create "${parts[@]}" -output "$APP/Contents/MacOS/$tool"
    chmod +x "$APP/Contents/MacOS/$tool"
  else
    cp "${parts[0]}" "$APP/Contents/MacOS/$tool"
    chmod +x "$APP/Contents/MacOS/$tool"
  fi
done

# Иконка .icns из PNG (sips/iconutil есть в macOS).
ICONSET="$OUT/icon.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Screen Recorder</string>
  <key>CFBundleDisplayName</key><string>Screen Recorder</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>screenrecorder</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleDevelopmentRegion</key><string>ru</string>
  <key>CFBundleShortVersionString</key><string>${VERSION%%-*}</string>
  <key>CFBundleVersion</key><string>${VERSION//[^0-9.]/}</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSScreenCaptureUsageDescription</key><string>Screen Recorder записывает экран для создания видео.</string>
  <key>NSMicrophoneUsageDescription</key><string>Screen Recorder записывает микрофон для озвучивания записи.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Screen Recorder записывает системный звук.</string>
  <key>NSHumanReadableCopyright</key><string>Freekazoid</string>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# Правка Info.plist после сборки требует переподписи. Для локальной
# разработки допустима ad-hoc подпись; для распространения задайте
# MACOS_SIGN_IDENTITY.
SIGN_IDENTITY="${MACOS_SIGN_IDENTITY:--}"
codesign --force --deep --sign "$SIGN_IDENTITY" \
  --entitlements "$ROOT/scripts/macos/entitlements.plist" "$APP"

ZIP="$OUT/ScreenRecorder-${VERSION}-macOS-${ARCH}.zip"
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"

if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
  xcrun notarytool submit "$ZIP" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
fi

if [[ "${CREATE_DMG:-0}" == "1" ]]; then
  DMG="$OUT/ScreenRecorder-${VERSION}-macOS-${ARCH}.dmg"
  rm -f "$DMG"
  hdiutil create -volname "Screen Recorder" -srcfolder "$APP" -ov -format UDZO "$DMG"
  echo "DMG: $DMG"
fi

echo "App: $APP"
echo "Zip: $ZIP"
