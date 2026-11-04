#!/usr/bin/env bash
# Генерирует эталонный клип для проверки синхронизации аудио и видео.
#
# В клипе два события: на 3-й и 10-й секундах одновременно появляется
# белая вспышка на весь кадр и звучит короткий сигнал 1 кГц. По разнице
# моментов вспышки и сигнала в записи вычисляется A/V offset.
set -euo pipefail

output="${1:-testdata/av-sync/av-sync-test.mp4}"
mkdir -p "$(dirname "$output")"

ffmpeg -hide_banner -loglevel warning -y \
    -f lavfi -i "color=c=0x101010:s=1280x720:r=30:d=14" \
    -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=14" \
    -filter_complex "
        [0:v]drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='between(t,3,3.05)+between(t,10,10.05)'[v];
        [1:a]volume='if(between(t,3,3.15)+between(t,10,10.15),0.7,0)':eval=frame[a]
    " \
    -map "[v]" -map "[a]" \
    -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p \
    -c:a aac -b:a 192k -shortest \
    -movflags +faststart \
    "$output"

printf 'Готово: %s\n' "$output"
printf 'Вспышки и сигналы: 3.0 с и 10.0 с\n'
