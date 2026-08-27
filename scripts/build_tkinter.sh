#!/usr/bin/env bash
# Build a CPython _tkinter extension linked against the SYSTEM Tcl/Tk 8.6
# (Xft + fontconfig). Needed because bundled interpreters (python-build-standalone,
# conda-forge) ship Tk without fontconfig: Cyrillic text renders ~70% wider.
#
# Requirements:
#   - .sdk/extracted/  (dev headers; see README or run scripts/fetch_tk_dev_headers.sh)
#   - .python/         (CPython 3.14 with matching headers)
#   - system packages: libtcl8.6, libtk8.6 (runtime libs)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK_INC="$ROOT_DIR/.sdk/extracted/usr/include"
OUT_DIR="$ROOT_DIR/.python/lib/python3.14/lib-dynload"
WORK_DIR="${TMPDIR:-/tmp}/opencode/tksrc"

PY_VER="$("$ROOT_DIR/.venv/bin/python" -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])')"
PY_TAG="$("$ROOT_DIR/.venv/bin/python" -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')"

mkdir -p "$WORK_DIR" && cd "$WORK_DIR"

# CPython sources (public + internal headers) and the pre-generated clinic file
if [[ ! -f "Python-${PY_VER}/Include/Python.h" ]]; then
  curl -sL -o cpython.tar.gz "https://www.python.org/ftp/python/${PY_VER}/Python-${PY_VER}.tgz"
  tar -xzf cpython.tar.gz "Python-${PY_VER}/Include"
fi
if [[ ! -f "clinic/_tkinter.c.h" ]]; then
  mkdir -p clinic
  curl -sL -o "clinic/_tkinter.c.h" \
    "https://raw.githubusercontent.com/python/cpython/v${PY_VER}/Modules/clinic/_tkinter.c.h"
fi
curl -sL -o "_tkinter.c" "https://raw.githubusercontent.com/python/cpython/v${PY_VER}/Modules/_tkinter.c"
curl -sL -o "tkinter.h" "https://raw.githubusercontent.com/python/cpython/v${PY_VER}/Modules/tkinter.h"

gcc -shared -fPIC -O2 \
  -I"Python-${PY_VER}/Include" -I"Python-${PY_VER}/Include/internal" \
  -I"$ROOT_DIR/.python/include/python${PY_VER%.*}" \
  -I"$SDK_INC/tcl8.6" -I"$SDK_INC" \
  _tkinter.c -o "_tkinter${PY_TAG}" \
  -L/usr/lib/x86_64-linux-gnu \
  -l:libtk8.6.so.0 -l:libtcl8.6.so.0

cp "_tkinter${PY_TAG}" "$OUT_DIR/"
echo "[build_tkinter] installed: $OUT_DIR/_tkinter${PY_TAG}"
