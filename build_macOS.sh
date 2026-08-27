#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$ROOT_DIR/output"
BUILD_DIR="$ROOT_DIR/build"
ENTRYPOINT="$ROOT_DIR/src/main.py"
APP_NAME="ScreenRecorder"
BUNDLE_ID="com.recordingscreen.app"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script must be run on macOS (Darwin)."
  exit 1
fi

if [[ -n "${APP_VERSION:-}" ]]; then
  VERSION="$APP_VERSION"
else
  VERSION="$(date +%Y.%m.%d-%H%M)"
fi

if [[ ! -f "$ENTRYPOINT" ]]; then
  echo "Entry point not found: $ENTRYPOINT"
  exit 1
fi

if [[ -x "$ROOT_DIR/.venv/bin/python" ]]; then
  PYTHON_BIN="$ROOT_DIR/.venv/bin/python"
else
  PYTHON_BIN="python3"
fi

if [[ -x "$ROOT_DIR/.venv/bin/pyinstaller" ]]; then
  PYINSTALLER="$ROOT_DIR/.venv/bin/pyinstaller"
elif command -v pyinstaller >/dev/null 2>&1; then
  PYINSTALLER="pyinstaller"
else
  echo "PyInstaller not found. Install it into .venv or globally."
  exit 1
fi

TARGET_ARCH="${MACOS_TARGET_ARCH:-$(uname -m)}"
case "$TARGET_ARCH" in
  arm64|x86_64|universal2) ;;
  *)
    echo "Unsupported MACOS_TARGET_ARCH: $TARGET_ARCH"
    echo "Use one of: arm64, x86_64, universal2"
    exit 1
    ;;
esac

EXTRA_ARGS=(
  --hidden-import "PIL._tkinter_finder"
  --hidden-import "dbus_next.aio"
  --hidden-import "dbus_next.aio.proxy_object"
  --collect-submodules "dbus_next"
  --collect-all "psutil"
  --hidden-import "psutil"
  --hidden-import "transformers.models.whisper"
  --hidden-import "transformers.models.whisper.modeling_whisper"
  --hidden-import "transformers.models.whisper.processing_whisper"
  --hidden-import "transformers.models.whisper.tokenization_whisper"
  --hidden-import "transformers.models.whisper.feature_extraction_whisper"
  --hidden-import "transformers.models.whisper.configuration_whisper"
  --collect-data "faster_whisper"
  --hidden-import "audio_transcriber_service"
  --hidden-import "background_tasks"
  --hidden-import "program_screen_wayland"
  --hidden-import "program_screen_win"
  --hidden-import "wasapi_loopback_record"
  --hidden-import "wayland_portal_async"
  --hidden-import "wayland_portal"
  --hidden-import "screencast_frame"
  --hidden-import "portal_identity"
  --hidden-import "settings_manager"
  --hidden-import "nemo_compat"
  --hidden-import "ffmpeg_pipewire"
  --hidden-import "merge_save"
  --target-arch "$TARGET_ARCH"
)

FFMPEG_MACOS_DIR="${FFMPEG_MACOS_DIR:-$ROOT_DIR/bin/macos}"
if [[ ! -f "$FFMPEG_MACOS_DIR/ffmpeg" ]]; then
  echo "[build] fetching static ffmpeg for macOS..."
  mkdir -p "$FFMPEG_MACOS_DIR"
  "$PYTHON_BIN" - "$FFMPEG_MACOS_DIR" <<'PYEOF'
import sys, os, shutil, urllib.request, tempfile, zipfile
from pathlib import Path
dest = Path(sys.argv[1])
# evermeet отдаёт только ffmpeg (без ffprobe); ffmpeg хватает для записи/склейки.
url = "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip"
with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tmp:
    with urllib.request.urlopen(url, timeout=120) as r:
        tmp.write(r.read())
    path = tmp.name
with zipfile.ZipFile(path) as z:
    for name in z.namelist():
        base = os.path.basename(name)
        if base == "ffmpeg" or base.startswith("ffmpeg"):
            with z.open(name) as src, open(dest / "ffmpeg", "wb") as dst:
                shutil.copyfileobj(src, dst)
os.unlink(path)
p = dest / "ffmpeg"
if p.exists():
    os.chmod(p, 0o755)
print("[build] macos ffmpeg fetched")
PYEOF
fi

if [[ -f "$FFMPEG_MACOS_DIR/ffmpeg" ]]; then
  EXTRA_ARGS+=(--add-binary "$FFMPEG_MACOS_DIR/ffmpeg:bin")
else
  echo "[build] ffmpeg not found in $FFMPEG_MACOS_DIR (fallback to system PATH at runtime)"
fi

if [[ -f "$FFMPEG_MACOS_DIR/ffprobe" ]]; then
  EXTRA_ARGS+=(--add-binary "$FFMPEG_MACOS_DIR/ffprobe:bin")
fi

if "$PYTHON_BIN" -c "import importlib.metadata; importlib.metadata.version('torchcodec')" >/dev/null 2>&1; then
  EXTRA_ARGS+=(--copy-metadata "torchcodec")
fi

# NeMo Sortformer (диаризация): см. комментарии в build_AppImage.sh.
if "$PYTHON_BIN" -c "import nemo" >/dev/null 2>&1; then
  "$PYTHON_BIN" "$ROOT_DIR/src/nemo_stubs/install_nemo_stub.py"
  EXTRA_ARGS+=(
    --collect-all "cuda"
    --collect-submodules "nemo"
    --collect-data "nemo_toolkit"
    --collect-data "lightning_fabric"
    --exclude-module "triton"
    --copy-metadata "nemo_toolkit"
    --copy-metadata "omegaconf"
    --copy-metadata "hydra_core"
    --copy-metadata "lightning"
    --copy-metadata "lhotse"
    --copy-metadata "pyannote.core"
    --copy-metadata "torch"
    --copy-metadata "torchaudio"
  )
fi

if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
  EXTRA_ARGS+=(--codesign-identity "$MACOS_SIGN_IDENTITY")
fi

if [[ -n "${MACOS_ENTITLEMENTS_FILE:-}" ]]; then
  if [[ ! -f "$MACOS_ENTITLEMENTS_FILE" ]]; then
    echo "Entitlements file not found: $MACOS_ENTITLEMENTS_FILE"
    exit 1
  fi
  EXTRA_ARGS+=(--osx-entitlements-file "$MACOS_ENTITLEMENTS_FILE")
fi

mkdir -p "$OUTPUT_DIR"

BIN_NAME="${APP_NAME}_v${VERSION}"
APP_BUNDLE="${BIN_NAME}.app"
APP_PATH="$OUTPUT_DIR/$APP_BUNDLE"
ZIP_NAME="${BIN_NAME}_macOS.zip"
ZIP_PATH="$OUTPUT_DIR/$ZIP_NAME"

echo "Building ${APP_BUNDLE} (arch: ${TARGET_ARCH})"
"$PYINSTALLER" \
  --noconfirm \
  --windowed \
  "$ENTRYPOINT" \
  --name "$BIN_NAME" \
  --osx-bundle-identifier "$BUNDLE_ID" \
  --distpath "$OUTPUT_DIR" \
  --workpath "$BUILD_DIR" \
  --specpath "$ROOT_DIR" \
  --icon "$ROOT_DIR/assets/icon.png" \
  --add-data "$ROOT_DIR/assets:assets" \
  --add-data "$ROOT_DIR/settings.json:." \
  --add-data "$ROOT_DIR/src/default_settings.json:." \
  "${EXTRA_ARGS[@]}"

if [[ ! -d "$APP_PATH" ]]; then
  echo "Build failed: app bundle not found at $APP_PATH"
  exit 1
fi

rm -f "$ZIP_PATH"
ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$ZIP_PATH"

GENERATED_SPEC="$ROOT_DIR/${BIN_NAME}.spec"
if [[ -f "$GENERATED_SPEC" ]]; then
  rm -f "$GENERATED_SPEC"
fi
rm -rf "$BUILD_DIR"

echo "Build complete: $APP_PATH"
echo "Archive created: $ZIP_PATH"

if [[ "${CREATE_DMG:-0}" == "1" ]]; then
  if ! command -v hdiutil >/dev/null 2>&1; then
    echo "CREATE_DMG=1 set, but hdiutil not found."
    exit 1
  fi

  DMG_NAME="${BIN_NAME}_macOS.dmg"
  DMG_PATH="$OUTPUT_DIR/$DMG_NAME"
  rm -f "$DMG_PATH"
  hdiutil create -volname "$APP_NAME" -srcfolder "$APP_PATH" -ov -format UDZO "$DMG_PATH"
  echo "DMG created: $DMG_PATH"
fi
