#!/usr/bin/env bash
# Собирает логи и диагностику Screen Recorder в один архив.
set -euo pipefail

stamp="$(date +%Y%m%d-%H%M%S)"
out="${1:-/tmp/opencode/screenrecorder-logs-$stamp}"
mkdir -p "$out"

logs="$HOME/.local/share/screenrecorder/logs"
for file in app.log app.log.1 daemon.log; do
    if [[ -f "$logs/$file" ]]; then
        cp "$logs/$file" "$out/$file"
    fi
done

if [[ -x ./target/debug/screenrecorder ]]; then
    ./target/debug/screenrecorder --diagnose >"$out/diagnose.json" 2>"$out/diagnose.err" || true
elif command -v screenrecorder >/dev/null 2>&1; then
    screenrecorder --diagnose >"$out/diagnose.json" 2>"$out/diagnose.err" || true
fi

{
    ps -eo pid,ppid,stat,etime,rss,cmd | grep -E "[s]creenrecorder" || true
} >"$out/processes.txt"

if command -v wpctl >/dev/null 2>&1; then
    wpctl status >"$out/wpctl.txt" 2>&1 || true
fi

if command -v xrandr >/dev/null 2>&1; then
    xrandr --query >"$out/xrandr.txt" 2>&1 || true
fi

archive="$out.tar.gz"
tar -czf "$archive" -C "$(dirname "$out")" "$(basename "$out")"
printf 'Архив: %s\n' "$archive"
