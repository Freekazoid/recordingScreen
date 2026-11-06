#!/usr/bin/env bash
# Измеряет A/V offset по записи эталонного клипа.
#
# Видео: ищет самые яркие кадры (белые вспышки) через signalstats.
# Звук: ищет начало сигнала 1 кГц через silencedetect.
# Смещение = время вспышки − время сигнала. Положительное значение значит,
# что видео отстаёт от звука; отрицательное — что опережает.
set -euo pipefail

input="${1:?использование: check_av_sync.sh <запись.mkv>}"
if [[ ! -f "$input" ]]; then
    printf 'Файл не найден: %s\n' "$input" >&2
    exit 1
fi

# Порог яркости: тёмный фон клипа даёт YAVG около 16, белая вспышка — выше 200.
video_times="$(
    ffmpeg -hide_banner -nostdin -loglevel info -i "$input" -an \
        -vf "signalstats,metadata=print:key=lavfi.signalstats.YAVG" \
        -f null - 2>&1 |
        awk '
            /pts_time:/ {
                for (i = 1; i <= NF; i++) {
                    if ($i ~ /^pts_time:/) {
                        split($i, parts, ":")
                        time = parts[2]
                    }
                }
            }
            /lavfi.signalstats.YAVG=/ {
                split($0, parts, "=")
                if (parts[2] + 0 > 180) {
                    print time
                }
            }
        ' |
        awk 'NR == 1 || $1 - last > 0.5 { print; last = $1 }' |
        head -n 2
)"

# Сигнал громкий, поэтому порог тишины можно держать высоко: фоновый шум
# системы обычно заметно тише. Первые 0.5 с пропускаем на случай щелчка
# в начале записи.
audio_times="$(
    ffmpeg -hide_banner -nostdin -loglevel info -i "$input" -vn \
        -af "silencedetect=noise=-30dB:d=0.05" \
        -f null - 2>&1 |
        awk '
            /silence_end:/ {
                for (i = 1; i <= NF; i++) {
                    if ($i == "silence_end:") {
                        time = $(i + 1)
                    }
                }
                if (time + 0 > 0.5) {
                    print time
                }
            }
        ' |
        head -n 2
)"

if [[ -z "$video_times" || -z "$audio_times" ]]; then
    printf 'Не удалось найти вспышки или сигналы.\n' >&2
    printf 'Видео: %s\nЗвук: %s\n' "${video_times:-нет}" "${audio_times:-нет}" >&2
    exit 1
fi

paste <(printf '%s\n' "$video_times") <(printf '%s\n' "$audio_times") |
    awk '
        BEGIN {
            printf "%-10s %-12s %-12s %s\n", "Событие", "Видео, с", "Звук, с", "Смещение"
        }
        {
            offset = ($1 - $2) * 1000
            printf "%-10d %-12.3f %-12.3f %+.0f мс\n", NR, $1, $2, offset
            total += offset
            count++
        }
        END {
            if (count > 0) {
                printf "Среднее смещение: %+.0f мс\n", total / count
            }
        }
    '

printf '\nОриентир: до ±100 мс считается нормой для записи экрана.\n'
printf 'Если смещение больше, приложите вывод скрипта и запись.\n'
