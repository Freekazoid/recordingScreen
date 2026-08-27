#!/usr/bin/env bash
# Download dev headers (from Ubuntu debs) needed to build _tkinter against
# system Tcl/Tk 8.6 with Xft/fontconfig. Extracted into .sdk/extracted/.
# No sudo required.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK_DIR="$ROOT_DIR/.sdk"

mkdir -p "$SDK_DIR" && cd "$SDK_DIR"

PKGS=(
  tcl8.6-dev tk8.6-dev
  libx11-dev libxft-dev libxrender-dev libxext-dev libxcb1-dev
  x11proto-dev libxau-dev libxdmcp-dev
  libfontconfig-dev libfreetype-dev libexpat1-dev zlib1g-dev
)

apt-get download "${PKGS[@]}"
mkdir -p extracted
for d in ./*.deb; do dpkg -x "$d" extracted/ && rm -f "$d"; done

echo "[fetch_tk_dev_headers] headers in $SDK_DIR/extracted/usr/include"
