#!/usr/bin/env bash
# Собирает GPU-вариант ASR-воркера (CUDA) рядом с приложением.
set -euo pipefail

cd "$(dirname "$0")/.."

cuda_home="${CUDA_HOME:-/usr/local/cuda}"
if [[ ! -x "$cuda_home/bin/nvcc" ]]; then
    echo "CUDA не найдена: $cuda_home/bin/nvcc" >&2
    echo "Укажите CUDA_HOME или установите CUDA toolkit." >&2
    exit 1
fi

export CUDA_HOME="$cuda_home"
export CUDACXX="${CUDACXX:-$cuda_home/bin/nvcc}"
export PATH="$cuda_home/bin:$PATH"

cargo build -p screenrecorder-ml-worker --features cuda

binary="target/debug/screenrecorder-ml"
if [[ ! -f "$binary" ]]; then
    echo "Не найден $binary" >&2
    exit 1
fi

printf 'GPU-воркер собран: %s\n' "$binary"
printf 'Проверка: %s --help (или запуск распознавания в приложении)\n' "$binary"
