#!/usr/bin/env bash
# Full project environment bootstrap (no sudo required).
# Idempotent: safe to re-run after moving the project or on a fresh clone.
#
# Steps:
#   1. .python/  — CPython 3.14 (python-build-standalone), skipped if present
#   2. .venv/    — virtualenv + requirements.txt (torch from CPU index)
#   3. .sdk/     — dev headers for _tkinter build
#   4. _tkinter  — compiled against system Tcl/Tk 8.6 (Xft/fontconfig)
#      Without this Cyrillic text renders ~70% wider in bundled Tk builds.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PY_MAJOR=3.14
PSA_REPO="astral-sh/python-build-standalone"

log() { echo "[setup] $*"; }

# ─── 1. CPython into .python/ ─────────────────────────────────────────────────
if [[ -x ".python/bin/python3" ]]; then
  log ".python/ already exists, skipping interpreter download"
else
  log "downloading python-build-standalone ${PY_MAJOR}..."
  mkdir -p .python
  PSA_TAG="$(curl -fsSL "https://api.github.com/repos/${PSA_REPO}/releases/latest" | grep -oP '"tag_name":\s*"\K[^"]+')"
  ASSET="$(curl -fsSL "https://api.github.com/repos/${PSA_REPO}/releases/latest" | \
    grep -oP "\"name\":\s*\"\Kcpython-${PY_MAJOR}\.[0-9.]+\+${PSA_TAG}-x86_64-unknown-linux-gnu-install_only\.tar\.gz(?=\")" | head -1)"
  [[ -n "$ASSET" ]] || { log "ERROR: no matching CPython ${PY_MAJOR} asset in release ${PSA_TAG}"; exit 1; }
  curl -fsSL --retry 3 -o /tmp/psa.tar.gz "https://github.com/${PSA_REPO}/releases/download/${PSA_TAG}/${ASSET}"
  tar -xzf /tmp/psa.tar.gz -C /tmp
  # archive contains python/ directory
  cp -a /tmp/python/. .python/
  rm -rf /tmp/psa.tar.gz /tmp/python
  log "installed $(.python/bin/python3 --version)"
fi

# ─── 2. Virtualenv + dependencies ─────────────────────────────────────────────
if [[ ! -x ".venv/bin/python" ]] || [[ "$(readlink -f .venv/bin/python)" != "$(readlink -f .python/bin/python3)" ]]; then
  log "(re)creating .venv from .python"
  rm -rf .venv
  .python/bin/python3 -m venv .venv
fi
.venv/bin/pip install -q --upgrade pip wheel
log "installing CUDA torch..."
.venv/bin/pip install -q --index-url https://download.pytorch.org/whl/cu128 torch torchaudio
log "installing requirements.txt..."
.venv/bin/pip install -q -r requirements.txt

# ─── 3+4. Fixed _tkinter against system Tcl/Tk 8.6 ────────────────────────────
bash scripts/fetch_tk_dev_headers.sh
bash scripts/build_tkinter.sh

# ─── Verify font rendering ────────────────────────────────────────────────────
log "verifying Cyrillic font metrics..."
RESULT="$(.venv/bin/python - <<'EOF'
import tkinter
r = tkinter.Tk(); r.withdraw()
w = r.tk.call('font','measure','Arial 11','Запись экрана')
print(r.tk.call('info','patchlevel'), w)
EOF
)"
TCL_VER="$(echo "$RESULT" | awk '{print $1}')"
WIDTH="$(echo "$RESULT" | awk '{print $2}')"
if [[ "$TCL_VER" != 8.6.* ]]; then
  log "ERROR: tkinter still uses Tcl $TCL_VER (need 8.6 with fontconfig)"
  exit 1
fi
if (( WIDTH > 300 )); then
  log "ERROR: Cyrillic renders too wide (${WIDTH}px, expected ~170px) — fontconfig not active"
  exit 1
fi
log "OK: Tcl ${TCL_VER}, Cyrillic width ${WIDTH}px"
log "environment ready"
