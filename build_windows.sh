#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$ROOT_DIR/output"
ENTRYPOINT="$ROOT_DIR/src/main.py"
APP_NAME="ScreenRecorder"
DOCKER_IMAGE="${DOCKER_IMAGE:-batonogov/pyinstaller-windows:latest}"
CONTAINER_ROOT="/src"
CONTAINER_NAME="${APP_NAME,,}-win-builder-$(date +%s)"
CLEANUP_DOCKER_IMAGE="${CLEANUP_DOCKER_IMAGE:-1}"

if [[ -n "${APP_VERSION:-}" ]]; then
  VERSION="$APP_VERSION"
else
  VERSION="$(date +%Y.%m.%d-%H%M)"
fi

if [[ ! -f "$ENTRYPOINT" ]]; then
  echo "Entry point not found: $ENTRYPOINT"
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required for Windows build from Linux."
  exit 1
fi

WORK_DIR="/tmp/${APP_NAME}_windows_build_$(date +%s)"
cleanup() {
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  rm -rf "$WORK_DIR"

  if [[ "$CLEANUP_DOCKER_IMAGE" == "1" ]]; then
    docker image rm -f "$DOCKER_IMAGE" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

mkdir -p "$WORK_DIR"

if command -v rsync >/dev/null 2>&1; then
  rsync -a --delete \
    --exclude '.git' \
    --exclude '.venv' \
    --exclude 'output' \
    --exclude 'build' \
    --exclude '.temp_postprocess' \
    --exclude '.temp_segments' \
    --exclude 'models' \
    "$ROOT_DIR/" "$WORK_DIR/"
else
  cp -a "$ROOT_DIR/." "$WORK_DIR/"
  rm -rf "$WORK_DIR/.venv" "$WORK_DIR/output" "$WORK_DIR/build" "$WORK_DIR/.temp_postprocess" "$WORK_DIR/.temp_segments" "$WORK_DIR/models"
fi

if [[ ! -f "$WORK_DIR/requirements.txt" ]]; then
  echo "requirements.txt not found in copied project."
  exit 1
fi

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
  --collect-all "vosk"
  --additional-hooks-dir "$CONTAINER_ROOT/hooks"
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
  # NeMo Sortformer: см. комментарии в build_AppImage.sh.
  --collect-all "cuda"
  --collect-submodules "nemo"
  --collect-data "nemo_toolkit"
  --collect-data "lightning_fabric"
  --exclude-module "triton"
  --hidden-import "ffmpeg_pipewire"
  --hidden-import "merge_save"
)

FFMPEG_WINDOWS_DIR="${FFMPEG_WINDOWS_DIR:-$ROOT_DIR/bin/windows}"

# Статический ffmpeg для Windows: скачиваем в bin/windows, чтобы экзе не
# зависел от системного ffmpeg (иначе нет аудио и постобработки).
if [[ ! -f "$FFMPEG_WINDOWS_DIR/ffmpeg.exe" || ! -f "$FFMPEG_WINDOWS_DIR/ffprobe.exe" ]]; then
  echo "[build] fetching static ffmpeg for Windows..."
  mkdir -p "$FFMPEG_WINDOWS_DIR"
  TMP_ZIP="$(mktemp --suffix=.zip)"
  TMP_DIR="$(mktemp -d)"
  curl -fsSL --retry 3 -o "$TMP_ZIP" "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
  python3 - "$TMP_ZIP" "$TMP_DIR" <<'PYEOF'
import sys, zipfile, shutil, glob
zp, dst = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(zp) as z:
    for name in z.namelist():
        if name.endswith("/bin/ffmpeg.exe") or name.endswith("/bin/ffprobe.exe"):
            z.extract(name, dst)
PYEOF
  find "$TMP_DIR" -path "*/bin/ffmpeg.exe" -exec cp {} "$FFMPEG_WINDOWS_DIR/ffmpeg.exe" \;
  find "$TMP_DIR" -path "*/bin/ffprobe.exe" -exec cp {} "$FFMPEG_WINDOWS_DIR/ffprobe.exe" \;
  rm -rf "$TMP_ZIP" "$TMP_DIR"
fi

if [[ -f "$FFMPEG_WINDOWS_DIR/ffmpeg.exe" ]]; then
  EXTRA_ARGS+=(--add-binary "$CONTAINER_ROOT/bin/windows/ffmpeg.exe:bin")
else
  echo "[build] ffmpeg.exe not found in $FFMPEG_WINDOWS_DIR (fallback to system PATH at runtime)"
fi

if [[ -f "$FFMPEG_WINDOWS_DIR/ffprobe.exe" ]]; then
  EXTRA_ARGS+=(--add-binary "$CONTAINER_ROOT/bin/windows/ffprobe.exe:bin")
fi

mkdir -p "$OUTPUT_DIR"
mkdir -p "$WORK_DIR/output"

BIN_NAME="${APP_NAME}_v${VERSION}"
FINAL_NAME="${BIN_NAME}.exe"
FINAL_PATH="$OUTPUT_DIR/$FINAL_NAME"

echo "Building ${FINAL_NAME} for Windows x64 using Docker"
BUILD_CMD="cd '$CONTAINER_ROOT' && pip install -r requirements.txt && python src/nemo_stubs/install_nemo_stub.py && KMP_AFFINITY=disabled pyinstaller --noconfirm --onefile --windowed --clean src/main.py --name '$BIN_NAME' --distpath output --workpath /tmp/build --specpath '$CONTAINER_ROOT' --icon '$CONTAINER_ROOT/assets/icon.ico' --add-data '$CONTAINER_ROOT/assets;assets' --add-data '$CONTAINER_ROOT/settings.json;.' --add-data '$CONTAINER_ROOT/src/default_settings.json;.' ${EXTRA_ARGS[*]}"

docker create \
  --name "$CONTAINER_NAME" \
  --entrypoint /bin/bash \
  "$DOCKER_IMAGE" \
  -c "$BUILD_CMD" >/dev/null

docker cp "$WORK_DIR/." "$CONTAINER_NAME:$CONTAINER_ROOT"
docker start -a "$CONTAINER_NAME"

docker cp "$CONTAINER_NAME:$CONTAINER_ROOT/output/$FINAL_NAME" "$FINAL_PATH"

if [[ ! -f "$FINAL_PATH" ]]; then
  echo "Build failed: file not found at $FINAL_PATH"
  exit 1
fi

rm -rf "$ROOT_DIR"/*.spec "$WORK_DIR/build" "$WORK_DIR"/*.spec

echo "Build complete: $FINAL_PATH"

if [[ "$CLEANUP_DOCKER_IMAGE" == "1" ]]; then
  echo "Docker image removed: $DOCKER_IMAGE"
else
  echo "Docker image kept: $DOCKER_IMAGE"
fi
