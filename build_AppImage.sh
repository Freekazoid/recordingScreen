#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$ROOT_DIR/output"
BUILD_DIR="$ROOT_DIR/build"
ENTRYPOINT="$ROOT_DIR/src/main.py"
APP_NAME="ScreenRecorder"

if [[ -n "${APP_VERSION:-}" ]]; then
  VERSION="$APP_VERSION"
else
  VERSION="$(date +%Y.%m.%d-%H%M)"
fi

# Запекаем версию сборки в приложение для окна «О программе»
# (файл автогенерируется, git-игнорируется; в CI приходит из APP_VERSION)
cat > "$ROOT_DIR/src/_build_version.py" <<EOF
# Автогенерируется build_AppImage.sh при каждой сборке.
APP_VERSION = "${VERSION}"
EOF

if [[ ! -f "$ENTRYPOINT" ]]; then
  echo "Entry point not found: $ENTRYPOINT"
  exit 1
fi

if [[ -x "$ROOT_DIR/.venv/bin/pyinstaller" ]]; then
  PYINSTALLER="$ROOT_DIR/.venv/bin/pyinstaller"
else
  PYINSTALLER="pyinstaller"
fi

if [[ -x "$ROOT_DIR/.venv/bin/python" ]]; then
  PYTHON_BIN="$ROOT_DIR/.venv/bin/python"
else
  PYTHON_BIN="python3"
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
  --additional-hooks-dir "$ROOT_DIR/hooks"
  --hidden-import "vosk"
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
  --hidden-import "ffmpeg_pipewire"
  --hidden-import "merge_save"
)

FFMPEG_LINUX_DIR="${FFMPEG_LINUX_DIR:-$ROOT_DIR/bin/linux}"
if [[ ! -f "$FFMPEG_LINUX_DIR/ffmpeg" || ! -f "$FFMPEG_LINUX_DIR/ffprobe" ]]; then
  echo "[build] fetching static ffmpeg for Linux..."
  mkdir -p "$FFMPEG_LINUX_DIR"
  "$PYTHON_BIN" - "$FFMPEG_LINUX_DIR" <<'PYEOF'
import sys, os, shutil, tarfile, urllib.request, tempfile
from pathlib import Path
dest = Path(sys.argv[1])
url = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
with tempfile.NamedTemporaryFile(suffix=".tar.xz", delete=False) as tmp:
    with urllib.request.urlopen(url, timeout=120) as r:
        tmp.write(r.read())
    path = tmp.name
with tarfile.open(path, "r:*") as t:
    for member in t.getmembers():
        base = os.path.basename(member.name)
        if member.isfile() and base in {"ffmpeg", "ffprobe"}:
            with t.extractfile(member) as src, open(dest / base, "wb") as dst:
                shutil.copyfileobj(src, dst)
os.unlink(path)
for b in ("ffmpeg", "ffprobe"):
    p = dest / b
    if p.exists():
        os.chmod(p, 0o755)
print("[build] linux ffmpeg fetched")
PYEOF
fi

if [[ -f "$FFMPEG_LINUX_DIR/ffmpeg" ]]; then
  EXTRA_ARGS+=(--add-binary "$FFMPEG_LINUX_DIR/ffmpeg:bin")
else
  echo "[build] ffmpeg not found in $FFMPEG_LINUX_DIR (fallback to system PATH at runtime)"
fi

if [[ -f "$FFMPEG_LINUX_DIR/ffprobe" ]]; then
  EXTRA_ARGS+=(--add-binary "$FFMPEG_LINUX_DIR/ffprobe:bin")
fi

if "$PYTHON_BIN" -c "import importlib.metadata; importlib.metadata.version('torchcodec')" >/dev/null 2>&1; then
  EXTRA_ARGS+=(--copy-metadata "torchcodec")
fi

# NeMo Sortformer (диаризация): yaml-конфиги нужны restore_from при загрузке .nemo.
if "$PYTHON_BIN" -c "import nemo" >/dev/null 2>&1; then
  # Заглушка nv_one_logger...pytorch_lightning: пакета нет на PyPI, но
  # PyInstaller при анализе импортирует ветки nemo и без неё теряет
  # субмодули (включая sortformer_modules).
  "$PYTHON_BIN" "$ROOT_DIR/src/nemo_stubs/install_nemo_stub.py"
  EXTRA_ARGS+=(
    --hidden-import "nemo_compat"
    # NeMo тянет cuda-python (cuda.bindings): бинарные субмодули (cydriver
    # и др.) PyInstaller без явного указания не собирает.
    --collect-all "cuda"
    # Много ленивых импортов внутри nemo — нужны все субмодули.
    --collect-submodules "nemo"
    --collect-data "nemo_toolkit"
    --collect-data "lightning_fabric"
    # triton (@triton.jit) требует исходников, которых нет в заморозке;
    # нужен только для GPU n-gram LM, диаризация на CPU его не использует.
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

# Fail fast if portal FD support is missing (common cause of silent AppImage picker failure)
if ! "$PYTHON_BIN" -c "import importlib.metadata,inspect; from dbus_next.aio.message_bus import MessageBus; v=importlib.metadata.version('dbus-next'); assert 'negotiate_unix_fd' in inspect.signature(MessageBus.__init__).parameters, v" >/dev/null 2>&1; then
  echo "[build] ERROR: need dbus-next>=0.2.3 with negotiate_unix_fd (pip install 'dbus-next>=0.2.3,<0.3')"
  "$PYTHON_BIN" -c "import importlib.metadata; print('[build] dbus-next=', importlib.metadata.version('dbus-next'))" 2>/dev/null || true
  exit 1
fi

# Bundle Tcl/Tk shared libraries kept outside the system linker search path
# (python-build-standalone / uv-managed interpreters ship them in <prefix>/lib).
# Without this the frozen app dies at startup with:
#   ImportError: libtcl9.0.so: cannot open shared object file
TKINTER_SO="$("$PYTHON_BIN" -c "import _tkinter; print(_tkinter.__file__)" 2>/dev/null || true)"
# CUDA: собрать рантайм-библиотеки torch (nvidia-* пакеты), иначе в заморозке
# torch.cuda.is_available() возвращает False даже при наличии GPU на хосте.
# dist-имя пакета: nvidia-cuda-runtime-cu12 -> каталог nvidia_cuda_runtime_cu12.
NVIDIA_STEMS="$("$PYTHON_BIN" - <<'PYEOF'
import importlib.metadata as md
nvidia = sorted(d.metadata["Name"] for d in md.distributions()
                if (d.metadata.get("Name") or "").lower().startswith("nvidia-"))
for n in nvidia:
    print(n.replace("-", "_"))
PYEOF
)"
if [[ -n "$NVIDIA_STEMS" ]]; then
  echo "[build] CUDA: collecting nvidia runtime packages"
  while IFS= read -r stem; do
    [[ -z "$stem" ]] && continue
    EXTRA_ARGS+=(--collect-all "$stem")
  done <<< "$NVIDIA_STEMS"
fi

if [[ -n "$TKINTER_SO" && -f "$TKINTER_SO" ]]; then
  # Guard: _tkinter must link system Tcl/Tk 8.6 (Xft/fontconfig). Bundled Tk 9
  # builds lack fontconfig and render Cyrillic ~70% wider (huge fonts bug).
  if ldd "$TKINTER_SO" | grep -qE "libtk9|libtcl9"; then
    echo "[build] ERROR: _tkinter is linked against Tcl/Tk 9 (no fontconfig, huge Cyrillic fonts)."
    echo "[build] Run: bash scripts/setup_env.sh   (or scripts/fetch_tk_dev_headers.sh && scripts/build_tkinter.sh)"
    exit 1
  fi
  if ! ldd "$TKINTER_SO" | grep -q "libtk8"; then
    echo "[build] ERROR: _tkinter is not linked against system Tcl/Tk 8.6."
    echo "[build] Run: bash scripts/setup_env.sh"
    exit 1
  fi
  PY_LIB_DIR="$(dirname "$(dirname "$(dirname "$TKINTER_SO")")")"
  while read -r libname; do
    [[ -z "$libname" ]] && continue
    if [[ -f "$PY_LIB_DIR/$libname" ]]; then
      echo "[build] bundling Tcl/Tk runtime library: $libname"
      EXTRA_ARGS+=(--add-binary "$PY_LIB_DIR/$libname:.")
    else
      echo "[build] ERROR: unresolved Tcl/Tk library '$libname' not found in $PY_LIB_DIR"
      exit 1
    fi
  done < <(ldd "$TKINTER_SO" 2>/dev/null | awk '/not found/ {print $1}' | sort -u)
fi

mkdir -p "$OUTPUT_DIR"

BIN_NAME="${APP_NAME}_v${VERSION}"
FINAL_NAME="${BIN_NAME}.AppImage"

echo "Building ${BIN_NAME}"
# --onedir (а не --onefile): 4ГБ payload не распаковывается в /tmp при каждом
# запуске. Squashfs из AppImage монтируется напрямую: мгновенный старт и
# отсутствие сбоев «decompression failed» при нехватке места в tmpfs.
"$PYINSTALLER" \
  --noconfirm \
  --onedir \
  --windowed \
  "$ENTRYPOINT" \
  --name "$BIN_NAME" \
  --distpath "$OUTPUT_DIR" \
  --workpath "$BUILD_DIR" \
  --specpath "$ROOT_DIR" \
  --icon "$ROOT_DIR/assets/icon.png" \
  --add-data "$ROOT_DIR/assets:assets" \
  --add-data "$ROOT_DIR/src/default_settings.json:." \
  "${EXTRA_ARGS[@]}"

if [[ -d "$OUTPUT_DIR/$BIN_NAME" && -f "$OUTPUT_DIR/$BIN_NAME/$BIN_NAME" ]]; then
  :
else
  echo "[build] ERROR: PyInstaller output not found: $OUTPUT_DIR/$BIN_NAME/"
  exit 1
fi

# CUDA-сборка большая — чистим промежуточный workpath до упаковки в AppImage,
# иначе на GitHub-раннере (14 ГБ) кончается место: onedir ~7 ГБ + squashfs ~4 ГБ.
rm -rf "$BUILD_DIR"

# ─── Package as a real AppImage (squashfs + runtime) ──────────────────────────
APPDIR="$OUTPUT_DIR/$APP_NAME.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mv "$OUTPUT_DIR/$BIN_NAME" "$APPDIR/usr/bin/$APP_NAME"
chmod +x "$APPDIR/usr/bin/$APP_NAME/$BIN_NAME"
# Единое имя бинарника внутри AppDir (пути к _internal относительные — ок)
mv "$APPDIR/usr/bin/$APP_NAME/$BIN_NAME" "$APPDIR/usr/bin/$APP_NAME/$APP_NAME"
ln -sf "usr/bin/$APP_NAME/$APP_NAME" "$APPDIR/AppRun"

cat > "$APPDIR/$APP_NAME.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Screen Recorder
Comment=Запись экрана с транскрипцией и диаризацией
Exec=AppRun
Icon=icon
Categories=AudioVideo;Recorder;
Terminal=false
EOF

cp "$ROOT_DIR/assets/icon.png" "$APPDIR/icon.png"
cp "$ROOT_DIR/assets/icon.png" "$APPDIR/.DirIcon"

APPIMAGETOOL="$ROOT_DIR/tools/appimagetool-x86_64.AppImage"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
if [[ ! -f "$APPIMAGETOOL" ]]; then
  echo "[build] downloading appimagetool..."
  mkdir -p "$(dirname "$APPIMAGETOOL")"
  curl -fsSL --retry 3 -o "$APPIMAGETOOL" "$APPIMAGETOOL_URL"
fi
chmod +x "$APPIMAGETOOL"

echo "[build] packaging AppImage with appimagetool..."
"$APPIMAGETOOL" --appimage-extract-and-run --no-appstream "$APPDIR" "$OUTPUT_DIR/$FINAL_NAME"

rm -rf "$APPDIR"

rm -rf "$BUILD_DIR" "$ROOT_DIR"/*.spec

echo "Build complete: $OUTPUT_DIR/$FINAL_NAME"
